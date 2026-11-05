# Arquitectura del motor de subastas

Decisiones técnicas del backend y cómo escala. Complementa [`SPEC.md`](../SPEC.md) (Parte 1 y
Parte 4) y la especificación v3 en [`docs/`](.).

## Decisiones

| Tema | Decisión | Por qué |
|---|---|---|
| Lenguaje | **Rust** (stable, edición 2021) | Performance y seguridad de memoria; `unsafe` prohibido en el workspace. |
| Base de datos | **PostgreSQL 16** | Relacional, transaccional, con backups en caliente, PITR y réplicas de lectura nativas. |
| Acceso a datos | `sqlx` 0.8 con consultas parametrizadas | Async, sin ORM; SQL explícito para controlar bloqueos. |
| Importes | `bigint` en unidades menores | Dinero exacto, sin flotantes (RT-04). |
| Reloj | `clock_timestamp()` de la primaria | Un solo reloj para todas las instancias (RT-03). |
| Eventos | Outbox transaccional | No se pierden avisos entre la puja y su envío (§5.1). |
| API | **axum 0.8** + **utoipa 5** | Mismo runtime (tokio) que `sqlx`; WebSocket incluido; middleware de `tower`. Rutas y OpenAPI salen del mismo código. |
| Autenticación | JWT HS256 verificado en la API | Sin sesión en el servidor. La habilitación para pujar se revalida en la base en cada puja. |

## Crates

```
crates/
├── remates-engine   Dominio puro: reglas, pujas, máximos, alargue, cierre. Sin IO.
├── remates-store    PostgreSQL: migraciones, transacciones, idempotencia, outbox.
└── remates-api      HTTP + WebSocket (axum 0.8), contrato OpenAPI (utoipa 5), JWT.
```

El motor no conoce la base ni el reloj: recibe `now` y devuelve los efectos de cada acción.
Eso permite probarlo con miles de secuencias aleatorias (proptest) y reutilizarlo igual desde
web, app o WhatsApp: **es la única autoridad de adjudicación** para todos los canales.

## Cómo se procesa una puja

Todo ocurre en **una transacción** sobre la primaria:

```
BEGIN
  SELECT … FROM auctions WHERE id = $1 FOR UPDATE   -- bloqueo por subasta (RT-01)
  SELECT clock_timestamp()                           -- reloj autoritativo (RT-03)
  ¿existe (bidder_id, request_id)? → devolver decisión guardada (RT-02)
  ¿usuario activo y habilitado? (FOR SHARE)
  motor.apply(comando, now)
  UPDATE auctions / UPSERT auction_bidders / INSERT bids, max_bids
  INSERT bid_requests (decisión, aceptada o rechazada)
  INSERT outbox (público con alias, avisos privados, bid.decision)
COMMIT
```

- **Orden único entre canales (RF-14).** `last_seq` se incrementa dentro del bloqueo; web,
  app y WhatsApp comparten la misma secuencia por subasta.
- **Pujas simultáneas del mismo importe.** La primera en tomar el bloqueo gana; la segunda ve
  el mínimo actualizado y recibe `BelowMinimum`. Probado con 25 pujas concurrentes.
- **Puja contra cierre.** El cierre toma el mismo bloqueo, así que una puja y el cierre de la
  misma subasta nunca se cruzan.
- **Idempotencia.** La clave la genera el cliente por intento. Un reintento después de un
  corte devuelve la misma decisión, y la misma clave con otro contenido es un error.
- **Append-only.** `bids`, `max_bids`, `bid_requests` y `awards` rechazan `UPDATE`/`DELETE`
  por trigger.

## Máximos automáticos (propuesta, P-04)

La posición se recalcula en forma cerrada después de cada acción:

- **Líder:** el mayor máximo; en empate, el que llegó primero.
- **Precio:** `min(máximo del líder, segundo máximo + incremento)`, nunca menos que la puja
  manual del líder.

No se itera escalón por escalón, así que el costo no depende del tamaño del incremento.

Las reglas todavía pendientes con el cliente son configurables por subasta y están marcadas
en el código:
- primera oferta en un remate al martillo (`FirstBidRule`, RT-05);
- fórmula del alargue (`ExtensionMode`, RF-12).

Lo que todavía **no** está implementado porque falta la regla de negocio:
- la anulación de ofertas (RF-13);
- la baja o cancelación de un máximo;
- el ingreso de pujas desde WhatsApp (P-01). La API ya acepta `Source::Whatsapp` y `Source::Admin`.

## API y tiempo real

- **Contrato primero.** Cada ruta se declara con `utoipa-axum`, así que si existe está
  documentada. `docs/openapi.json` se versiona en el repo y la CI falla si quedó desactualizado.
  Sirve para generar los clientes de la web y de la futura app.
- **El postor sale del token**, nunca del cuerpo del pedido. El rol del token habilita rutas;
  la base decide si el usuario puede pujar.
- **Pujas por HTTP, difusión por WebSocket.** El socket es de sólo lectura, así que las pujas
  siempre pasan por el camino idempotente.
- **Difusión con varias instancias sin broker.** Cada instancia lee *todo* el outbox (sólo
  lectura, desde la réplica si la hay) y lo reparte a sus propios sockets. La despierta
  `LISTEN outbox` en la primaria, con una consulta de respaldo cada 500 ms.
- **Ids del outbox fuera de orden.** Los `bigserial` pueden confirmarse desordenados: el
  lector espera hasta 3 s a que se complete un hueco antes de darlo por perdido. Si igual se
  pierde un evento, el cliente lo detecta por la `version` y pide `resync`.
- **Cierre automático** cada segundo en cada instancia (`RUN_WORKERS`). Puede correr en todas
  a la vez.
- **Límites:** timeout de 10 s por pedido (un reintento con la misma clave da la decisión
  real), cuerpo de hasta 16 KB, id de pedido (`x-request-id`) en cada respuesta y log. Los
  logs no incluyen la query, que puede llevar el token del WebSocket.

Pendiente en esta capa:
- límite de pedidos por usuario e IP (por ejemplo `tower_governor`);
- métricas Prometheus;
- un consumidor del outbox para los avisos externos (WhatsApp, correo).

Hoy los eventos quedan sin marcar como publicados hasta que exista ese consumidor.

## Escalado

### Hoy: una primaria, sin réplicas

- **Backend sin estado.** Se pueden correr N instancias detrás de un balanceador desde el día
  uno: la coordinación está en los bloqueos de Postgres, no en memoria.
- **Paralelismo por subasta.** El bloqueo es por fila de `auctions`. Subastas distintas se
  procesan en paralelo; sólo se serializan las pujas de una misma subasta, que es justo lo
  que hay que serializar.
- **Cierres en paralelo.** `close_due` usa `FOR UPDATE SKIP LOCKED`: varios procesos reparten
  las subastas vencidas sin pisarse. Probado con 4 procesos y 12 subastas.
- **Publicadores en paralelo.** El outbox también usa `SKIP LOCKED`, y un trigger hace
  `pg_notify('outbox')` para despertarlos sin consultar en bucle.
- **Certificados sin huecos.** Contador con bloqueo de fila (`counters`). Es el único punto
  global; los cierres son poco frecuentes, así que no es un cuello de botella.

### Réplicas de lectura

Ya preparado: `Db` tiene un pool `primary` y otro `replica`. Si `DATABASE_REPLICA_URL` no está
definida, la "réplica" es la primaria.

| Va a la primaria | Puede ir a la réplica |
|---|---|
| Pujas, máximos, cierres, idempotencia | Catálogo, posición informativa (`standing`), historial |
| Cualquier lectura que decide algo | Exportaciones, reportes, paneles |

Para sumar réplicas: replicación por streaming de Postgres, definir `DATABASE_REPLICA_URL`
(o un balanceador de réplicas) y listo. La réplica puede estar atrasada unos milisegundos: por
eso **nunca** se usa para validar ni para leer lo que uno acaba de escribir. La posición en
vivo de la subasta la empuja el tiempo real a partir del outbox, no la réplica.

### Más adelante

1. **Pool de conexiones** (PgBouncer en modo transacción) cuando haya muchas instancias.
   Requiere PgBouncer ≥ 1.21 por los prepared statements de `sqlx`.
2. **Particionado** de `bids`, `bid_requests` y `outbox` por tiempo cuando crezcan; el outbox
   publicado se puede purgar.
3. **Sharding por `auction_id`.** Toda transacción del motor toca una sola subasta, así que
   el motor ya es *shard-local*: se puede repartir subastas entre varias primarias (Citus o
   un mapa de shards en la aplicación). Sólo los contadores globales (certificados)
   necesitarían rangos por shard o un servicio propio.

## Backups

- **Continuos:** backup base (`pg_basebackup` o pgBackRest) + archivado de WAL. Permite
  restaurar a cualquier instante (PITR).
- **Lógicos:** `pg_dump` periódico como segunda línea y para exportar.
- **Prueba de restauración mensual** (requisito de S1/RF-33). Restaurar en un entorno aparte y
  correr las verificaciones del motor: `Auction::from_state` valida los invariantes de cada
  subasta al cargarla.
- Los objetivos de RPO y RTO no están en las fuentes y hay que acordarlos (P-06).

## Seguridad

- Ningún evento público lleva identidades ni máximos: sólo alias (RF-13, §5.5). Lo verifica
  una prueba.
- El historial con identidades reales (`admin_history`) es sólo para administradores, y la API
  debe restringirlo.
- Habilitación del postor verificada en la misma transacción (`users.status`, `users.can_bid`).
