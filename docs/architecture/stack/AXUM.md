# axum

**Qué es:** framework web para Rust, mantenido por el equipo de `tokio`. Se apoya en `hyper`
(HTTP) y en `tower` (middleware).
**Versión:** `axum` 0.8.9, con las features `ws` y `macros` (ver `Cargo.lock`).
**Dónde se usa:** sólo en `remates-api`. El motor y el store no lo conocen.

Ver también: [`SQLX.md`](SQLX.md), [`UTOIPA.md`](UTOIPA.md) y
[`../COMPONENTS.md`](../COMPONENTS.md#5-remates-api-por-dentro).

---

## Para qué lo usamos

| Necesidad | Qué aporta axum | Dónde |
|---|---|---|
| API REST `/v1` | Router, handlers `async` y extracción tipada de path, query, headers y cuerpo JSON | `routes/auctions.rs`, `routes/admin.rs` |
| WebSocket por subasta | `WebSocketUpgrade` y `WebSocket`, con la feature `ws` | `routes/ws.rs` |
| Autenticación | Extractores propios: un handler que pide `AuthUser` o `AdminUser` no corre sin un token válido | `auth.rs` |
| Estado compartido | `State<AppState>`, clonado barato en cada pedido | `state.rs` |
| Errores uniformes | `IntoResponse` para `ApiError`: todo error sale como `ErrorBody` JSON | `error.rs` |
| Middleware | Cualquier capa de `tower` / `tower-http`: timeout, trazas, request id, CORS y límite de cuerpo | `lib.rs::app` |
| Apagado ordenado | `axum::serve(...).with_graceful_shutdown(...)` | `main.rs` |
| Pruebas sin red | El `Router` es un `tower::Service`: las pruebas lo llaman con `oneshot` | `tests/api.rs` |

---

## Por qué axum

La decisión está registrada en [`../../ARCHITECTURE.md`](../../ARCHITECTURE.md#decisiones):
*"Mismo runtime (tokio) que `sqlx`; WebSocket incluido; middleware de `tower`. Rutas y OpenAPI
salen del mismo código."* En detalle:

1. **Mismo runtime que la base.** `sqlx`, `tokio::sync::broadcast` (en el `Hub`),
   `CancellationToken` y los procesos de fondo corren todos sobre `tokio`. axum no agrega un
   modelo de ejecución propio: un handler es una función `async` común.
2. **El middleware es `tower`.** Timeout, trazas, CORS y request id son capas de `tower-http`
   ya probadas, no código nuestro. La misma abstracción sirve para lo que falta: el límite de
   pedidos (por ejemplo `tower_governor`) y las métricas.
3. **WebSocket incluido.** El tiempo real (RF-15) es parte del producto, no un agregado. Con
   axum, HTTP y WebSocket comparten router, estado, autenticación y middleware.
4. **Los permisos van en la firma del handler.** Si un handler recibe `AdminUser`, es
   imposible ejecutarlo sin rol admin. El chequeo no depende de acordarse de llamar a una
   función; lo garantiza el compilador.
5. **Se integra con `utoipa-axum`.** Cada ruta se registra junto con su documentación, así que
   no puede existir una ruta sin documentar (ver [`UTOIPA.md`](UTOIPA.md)).
6. **Ecosistema y mantenimiento.** Es el framework web de Rust con más uso y lo mantiene el
   mismo equipo que `tokio`, `hyper` y `tower`.

### Alternativas consideradas

El repo no registra una comparación formal. Esto es lo que pesa en contra de cada alternativa,
frente a las necesidades de arriba:

| Alternativa | Qué ofrece | Por qué no |
|---|---|---|
| **actix-web** | Muy madura y rápida. | Middleware propio, no `tower`, así que no se reutiliza `tower-http`. Corre cada worker en un hilo, con tareas que no necesitan ser `Send`: es otro modelo mental al lado de `sqlx` y del `Hub`. La diferencia de rendimiento no importa acá: el cuello de botella es el bloqueo por subasta en Postgres, no el framework. |
| **poem** + `poem-openapi` | OpenAPI integrado desde el diseño. | Comunidad más chica; atar el contrato de la API a un framework menos usado es más riesgoso que usar `utoipa`, que es independiente del framework. |
| **Rocket** | Ergonomía alta. | Ritmo de versiones lento y WebSocket fuera del núcleo. |
| **warp** | También sobre `hyper`. | El estilo de *filters* da tipos y errores de compilación difíciles de leer, y hoy el ecosistema creció alrededor de axum. |
| **hyper** directo | Control total. | Habría que reescribir router, extractores, WebSocket y middleware. |

---

## Cómo lo usamos

### Handlers: la firma es el contrato

```rust
pub async fn place_bid(
    State(st): State<AppState>,   // estado compartido
    user: AuthUser,               // 401 si no hay token válido
    headers: HeaderMap,           // x-client-channel
    Path(id): Path<Uuid>,         // 400 si no es un UUID
    Json(body): Json<BidBody>,    // 400/415/422 si el cuerpo no es válido
) -> Result<Json<DecisionResponse>, ApiError> { … }
```

Convenciones:

- **El postor sale del token, nunca del cuerpo.** `BidBody` no tiene campo de usuario.
- **El cuerpo va último.** axum exige que el extractor que consume el cuerpo (`Json`) sea el
  último argumento.
- **Una puja rechazada no es un error HTTP.** Responde 200 con `decision.status = "rejected"`.
  `ApiError` queda para problemas del pedido: 401, 403, 404, 409 y 422.

### Extractores propios

`AuthUser` implementa `FromRequestParts<AppState>`: lee `Authorization: Bearer …` y lo
verifica con `JwtAuth`. `AdminUser` lo envuelve y además exige `Role::Admin`. En el
WebSocket, `OptionalUser` acepta también `?access_token=`, porque los navegadores no pueden
mandar cabeceras al abrir un socket.

El rol del token **sólo habilita rutas**. Si el usuario puede pujar lo decide la base en la
misma transacción de la puja (`users.status`, `users.can_bid`).

### Pila de middleware

Se arma en `lib.rs::app`. Las capas se listan de adentro hacia afuera; un pedido las atraviesa
en orden inverso:

| Capa | Origen | Para qué |
|---|---|---|
| `DefaultBodyLimit::max(16 KB)` | axum | Las pujas son chicas; corta cuerpos grandes antes de deserializar. |
| `SetRequestIdLayer` | tower-http | Genera `x-request-id` si no viene en el pedido. |
| `TraceLayer` | tower-http | Un *span* por pedido con método, **sólo el path** y request id. La query no se registra porque puede llevar `access_token`. |
| `PropagateRequestIdLayer` | tower-http | Devuelve el `x-request-id` en la respuesta. |
| `TimeoutLayer` (10 s → 408) | tower-http | Un pedido nunca queda colgado. Si una puja vence por tiempo, el cliente la reintenta con el mismo `request_id` y recibe la decisión real. |
| `CorsLayer` | tower-http | Sólo si `CORS_ALLOWED_ORIGINS` tiene valores. Permite `GET`/`POST` y las cabeceras que usa la API. |

### WebSocket

`auction_ws` se suscribe al `Hub` **antes** de leer el snapshot (así ningún evento cae en el
medio) y después responde `101`. La sesión corre en su propia tarea, así que el `TimeoutLayer`
de 10 s no la corta. Un `tokio::select!` atiende tres cosas: eventos del `Hub`, mensajes del
cliente (`resync`) y un ping cada 20 s. Detalle del protocolo en
[`../COMPONENTS.md`](../COMPONENTS.md#5-remates-api-por-dentro).

### Pruebas

`tests/api.rs` construye la aplicación completa, con el middleware, sobre una base temporal
(`#[sqlx::test]`) y la llama con `tower::ServiceExt::oneshot`, sin abrir puertos. Las pruebas
de WebSocket sí levantan un servidor real y se conectan con `tokio-tungstenite`.

---

## Costos y riesgos

| Riesgo | Mitigación |
|---|---|
| **Es 0.x.** Puede haber cambios incompatibles entre versiones menores. Por ejemplo, la 0.8 cambió la sintaxis de rutas de `/:id` a `/{id}`. | Versiones fijadas en `Cargo.lock`. Las actualizaciones se hacen a propósito, con la suite de `tests/api.rs` como red. |
| **Los errores de compilación de los handlers son opacos.** Un argumento que no es extractor da un error sobre `Handler` difícil de leer. | La feature `macros` ya está activa: `#[axum::debug_handler]` sobre el handler da un mensaje claro. |
| **El WebSocket no tiene control de flujo por cliente.** | El `Hub` usa un `broadcast` con capacidad 256: si un cliente se atrasa más, recibe un snapshot nuevo en lugar de acumular memoria. |
| **No hay límite de pedidos por usuario ni por IP.** | Pendiente. Se agrega como otra capa `tower` (ver [`../../ARCHITECTURE.md`](../../ARCHITECTURE.md#api-y-tiempo-real)). |

## Cuándo reconsiderarlo

- Si el tiempo real necesitara otro transporte (por ejemplo, WebTransport) que axum no soporte.
- Si `tower` dejara de ser el estándar de middleware del ecosistema `tokio`.

Ninguna de las dos cosas pasa hoy. Cambiar de framework, además, sólo afectaría a
`remates-api`: el motor y el store no dependen de axum.
