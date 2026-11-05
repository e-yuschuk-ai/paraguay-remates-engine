# sqlx

**Qué es:** biblioteca `async` para hablar con bases SQL desde Rust. No es un ORM: se escribe
SQL y sqlx se encarga de conexiones, pools, transacciones, parámetros, mapeo de filas,
migraciones y `LISTEN/NOTIFY`.
**Versión:** `sqlx` 0.8.6, sin las features por defecto. Se activan: `runtime-tokio`,
`tls-rustls`, `postgres`, `uuid`, `chrono`, `json`, `migrate`, `macros` y `derive`.
**Dónde se usa:** en `remates-store` (todo el acceso a datos) y, en menor medida, en
`remates-api`: `/ready`, el `PgListener` del `Hub` y la traducción de errores.

Ver también: [`AXUM.md`](AXUM.md), [`UTOIPA.md`](UTOIPA.md) y
[`../COMPONENTS.md`](../COMPONENTS.md#4-remates-store-por-dentro).

---

## Para qué lo usamos

| Necesidad del motor | Qué aporta sqlx | Dónde |
|---|---|---|
| Una transacción por cambio de subasta (RT-01) | `pool.begin()` → `Transaction<'_, Postgres>`, con `commit` y `rollback` explícitos | `store.rs::submit`, `close_auction`, `close_due` |
| Bloqueos de fila: `FOR UPDATE`, `FOR SHARE`, `SKIP LOCKED` | Ejecuta SQL tal cual; no hay una capa que lo esconda | `rows.rs::load_auction`, `store.rs`, `outbox.rs` |
| Reloj autoritativo (RT-03) | `query_scalar("SELECT clock_timestamp()")` dentro de la transacción | `clock.rs` |
| Primaria y réplica | Dos `PgPool` con `PgPoolOptions` (tamaño máximo y timeout de 5 s para conseguir conexión) | `db.rs` |
| Despertar la difusión | `PgListener` sobre el canal `outbox` (`pg_notify` desde un trigger) | `outbox.rs::listen`, `hub.rs` |
| Migraciones | `sqlx::migrate!("./migrations")` las embebe en el binario; se aplican al arrancar con `RUN_MIGRATIONS=true` | `db.rs::migrate` |
| Idempotencia y errores de integridad | `DatabaseError::is_unique_violation()` / `is_foreign_key_violation()` | `store.rs::is_request_conflict`, `api/error.rs` |
| Filas → tipos de Rust | `#[derive(sqlx::FromRow)]` | `rows.rs`, `outbox.rs::OutboxEvent` |
| Pruebas contra Postgres real | `#[sqlx::test(migrations = …)]` crea una base nueva por prueba, aplica las migraciones y la borra al terminar | `tests/store.rs`, `remates-api/tests/api.rs` |

---

## Por qué sqlx

La decisión registrada en [`../../ARCHITECTURE.md`](../../ARCHITECTURE.md#decisiones) es:
*"Async, sin ORM; SQL explícito para controlar bloqueos."* En detalle:

1. **La exactitud del motor depende del SQL.** El orden único entre canales (RF-14), que dos
   pujas iguales no se pisen y que una puja no se cruce con el cierre: todo eso sale de
   `SELECT … FOR UPDATE` sobre la fila de la subasta. Repartir cierres entre instancias sale de
   `FOR UPDATE SKIP LOCKED`, y la suspensión concurrente de un usuario, de `FOR SHARE`. Con un
   ORM esas cláusulas quedan escondidas o hay que forzarlas. Con sqlx, el SQL que se revisa
   en el código es el que corre.
2. **Es `async` sobre `tokio`.** Comparte runtime con axum y con el `Hub`. Esperar una
   conexión o un bloqueo no ocupa un hilo.
3. **Trae lo que en otras opciones son piezas sueltas.** Pool, TLS con `rustls` (sin
   OpenSSL), migraciones, `LISTEN/NOTIFY` y el arnés de pruebas vienen en una sola
   dependencia.
4. **El driver es Rust puro.** No necesita `libpq` en la imagen ni en la máquina de
   desarrollo: alcanza con `cargo build`.
5. **`#[sqlx::test]` aísla cada prueba.** Las pruebas de concurrencia (25 pujas simultáneas, 4
   procesos cerrando 12 subastas) corren en paralelo, cada una en su base.

### Alternativas consideradas

El repo no registra una comparación formal. Esto es lo que pesa en contra de cada alternativa:

| Alternativa | Qué ofrece | Por qué no |
|---|---|---|
| **Diesel** (+ `diesel-async`) | DSL con chequeo de tipos en compilación a partir del esquema. | El DSL se interpone justo donde el motor necesita precisión: bloqueos, `ON CONFLICT`, `clock_timestamp()`. El núcleo es síncrono; para `async` hace falta `diesel-async` y un pool aparte. |
| **SeaORM** | ORM *async* construido sobre sqlx. | Agrega un modelo de entidades que el dominio no usa: el estado vive en `remates-engine` y se persiste a mano. Esconde los bloqueos. |
| **tokio-postgres** (+ `deadpool`, `refinery`) | Driver *async* de bajo nivel, muy sólido. | Pool, migraciones, mapeo de filas y arnés de pruebas serían tres o cuatro dependencias más, y código propio para unirlas. |

---

## Cómo lo usamos

### Consultas en tiempo de ejecución, no macros `query!`

Todas las consultas usan `sqlx::query` / `query_as` / `query_scalar` con `.bind(...)`:

```rust
sqlx::query_scalar("SELECT status = 'active' AND can_bid FROM users WHERE id = $1 FOR SHARE")
    .bind(bidder.0)
    .fetch_optional(&mut *conn)
    .await?
```

**No** se usan las macros `query!` / `query_as!`, que validan el SQL contra una base real al
compilar. Es una decisión con costo:

| A favor | En contra |
|---|---|
| Compilar no requiere una base corriendo, ni una caché `.sqlx/` que regenerar con `cargo sqlx prepare` cada vez que cambia una consulta. | Un error de SQL o un tipo de columna mal mapeado aparece al ejecutar, no al compilar. |
| El SQL dinámico mínimo que existe es simple: `load_auction` agrega ` FOR UPDATE` sólo cuando se pide bloqueo. | |

La mitigación es que **cada consulta se ejecuta en las pruebas contra Postgres real**, en local
y en CI. Si el esquema crece mucho, conviene pasar a `query!` con `SQLX_OFFLINE=true` en CI
(ver [Cuándo reconsiderarlo](#cuándo-reconsiderarlo)).

**Ningún valor del usuario entra al texto SQL.** Todo pasa por `.bind`, que usa parámetros de
Postgres (`$1`, `$2`…). El único `format!` sobre SQL (`rows.rs`) concatena constantes del
código.

### Transacciones

El patrón de `Store::submit` es el núcleo del sistema:

```rust
let mut tx = self.db.primary().begin().await?;            // BEGIN en la primaria
let mut auction = self.lock_auction(&mut tx, id).await?;  // SELECT … FOR UPDATE
let now = self.clock.now(&mut tx).await?;                 // clock_timestamp()
// … idempotencia, habilitación, motor, escrituras, outbox …
tx.commit().await?;
```

- Si la función devuelve un error con `?` antes del `commit`, el `Transaction` se descarta y
  sqlx hace `ROLLBACK`. No queda nada a medias.
- Las funciones auxiliares reciben `&mut PgConnection` (`&mut *tx`), así se pueden usar dentro
  o fuera de una transacción.
- La violación de la clave única `(bidder_id, request_id)` puede aparecer recién en el
  `commit`. Por eso `is_request_conflict` se evalúa sobre el resultado del `commit` y se
  convierte en `StoreError::IdempotencyConflict` (409).

### Primaria y réplica

`Db` guarda dos pools. Sin `DATABASE_REPLICA_URL`, el de réplica es un clon del primario. La
regla está en el código (`db.rs`): **la réplica nunca valida, decide ni lee lo que se acaba de
escribir**. Sólo atiende `standing`, `public_history`, `admin_history` y la lectura de difusión
del outbox.

### `LISTEN/NOTIFY`

Un trigger hace `pg_notify('outbox')` en cada `INSERT`. El `Hub` mantiene un `PgListener`
contra la **primaria** (las notificaciones no se replican) y, al recibir un aviso, lee los
eventos nuevos de la réplica. Si la conexión de `LISTEN` se corta, la reabre y mientras tanto
consulta cada 500 ms.

### Migraciones

Están en `crates/remates-store/migrations/`, en SQL plano y numeradas (`0001_…`). `migrate!`
las compila dentro del binario, así el servidor no depende de archivos sueltos al desplegar.
Las migraciones aplicadas se registran en `_sqlx_migrations` con su checksum: **una migración
publicada no se edita**, se agrega una nueva.

---

## Costos y riesgos

| Riesgo | Mitigación |
|---|---|
| SQL sin verificar al compilar | Pruebas contra Postgres real en CI. Opción futura: `query!` en modo offline. |
| **Es 0.x.** La 0.9 puede traer cambios incompatibles. | Versión fijada en `Cargo.lock`; uso acotado a `remates-store`. |
| **Prepared statements y PgBouncer.** sqlx cachea sentencias preparadas por conexión, y PgBouncer en modo transacción las rompía antes de la 1.21. | Documentado en [`../../ARCHITECTURE.md`](../../ARCHITECTURE.md#más-adelante): usar PgBouncer ≥ 1.21. |
| **Tamaño del pool.** `DATABASE_MAX_CONNECTIONS` (20 por defecto) es por instancia y por pool: N instancias abren hasta 2 × N × 20 conexiones. | Ajustarlo con el `max_connections` de Postgres al escalar, o poner PgBouncer delante. |
| **Una transacción larga retiene el bloqueo de la subasta.** | El trabajo dentro de la transacción es sólo motor en memoria y unas pocas escrituras; nada de red externa. Lo que va hacia afuera (WhatsApp, correo) sale después, por el outbox. |

## Cuándo reconsiderarlo

- **Pasar a `query!`** cuando haya muchas consultas o varias personas tocando el esquema: el
  chequeo en compilación vale más que el costo de mantener `.sqlx/`.
- **Cambiar de biblioteca** sólo si hiciera falta algo que sqlx no cubre, como `COPY` masivo o
  el protocolo de replicación lógica. Para eso se puede sumar `tokio-postgres` en un punto
  aislado, sin reemplazar sqlx.
