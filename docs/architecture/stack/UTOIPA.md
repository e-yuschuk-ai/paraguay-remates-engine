# utoipa

**Qué es:** biblioteca que genera documentos **OpenAPI 3.1** a partir del código Rust, con
macros sobre los handlers (`#[utoipa::path]`) y sobre los tipos (`#[derive(ToSchema)]`).
**Versiones:** `utoipa` 5.5.0, `utoipa-axum` 0.2.0 (integración con el router de axum) y
`utoipa-scalar` 0.3.0 (la página `/docs`).
**Dónde se usa:** en `remates-api` (rutas y documento) y, **detrás de la feature `openapi`**,
en los tipos públicos de `remates-engine` y `remates-store`.

Ver también: [`AXUM.md`](AXUM.md), [`SQLX.md`](SQLX.md) y el contrato publicado en
[`../../openapi.json`](../../openapi.json).

---

## Para qué lo usamos

El contrato de la API es un entregable: de él salen los clientes de la web y de la futura app
móvil (DEC-01). utoipa cubre tres cosas:

| Necesidad | Cómo | Dónde |
|---|---|---|
| Que el contrato describa la API real | El documento se genera desde los mismos handlers y tipos que compila el servidor. | `lib.rs::api_router`, `routes/*.rs` |
| Publicarlo | `GET /openapi.json` lo sirve en vivo. `remates-api openapi` lo imprime sin conectarse a la base. | `lib.rs::app`, `main.rs` |
| Que se pueda leer | `GET /docs` muestra una referencia navegable con Scalar. | `lib.rs::app` |
| Que no se desactualice | El documento se versiona en `docs/openapi.json` y **la CI falla si difiere** del que genera el código. | `.github/workflows/ci.yml` |

---

## Por qué utoipa

La decisión registrada en [`../../ARCHITECTURE.md`](../../ARCHITECTURE.md#decisiones) es:
*"Rutas y OpenAPI salen del mismo código."* En detalle:

1. **Código primero, sin desfasajes.** Escribir el OpenAPI a mano y el servidor por separado
   termina en un contrato que miente. Con utoipa, el esquema de `BidBody` es el `struct
   BidBody`: si cambia un campo, cambia el documento.
2. **Con `utoipa-axum` no hay rutas sin documentar.** Las rutas se registran con
   `routes!(…)`, que toma el path y el método **de la anotación `#[utoipa::path]`**. Un handler
   sin anotación no compila en ese router. Así, para que exista una ruta, tiene que estar
   documentada.
3. **No depende del framework.** Los esquemas (`ToSchema`) no saben nada de axum. Si algún día
   se cambiara de framework, el trabajo sobre el contrato se conserva.
4. **El dominio no queda atado.** `remates-engine` y `remates-store` derivan `ToSchema` sólo
   con la feature `openapi` (`#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]`).
   Por defecto el motor no depende de utoipa, y `remates-api` activa la feature al usarlo.
5. **Soporte de OpenAPI 3.1 y adopción.** Es la biblioteca de OpenAPI más usada en Rust y
   genera 3.1, que se lleva mejor con JSON Schema (por ejemplo, para tipos que pueden ser
   nulos).

### Alternativas consideradas

El repo no registra una comparación formal. Esto es lo que pesa en contra de cada alternativa:

| Alternativa | Qué ofrece | Por qué no |
|---|---|---|
| **OpenAPI escrito a mano** (*spec-first*) | El contrato se discute antes del código. | Sin generadores de servidor Rust maduros, servidor y documento divergen. El chequeo en CI no tendría contra qué comparar. |
| **aide** | Generación desde axum, con esquemas de `schemars`. | Mucho menos uso; liga el documento a axum. |
| **poem-openapi** | El contrato es el propio sistema de rutas. | Obliga a usar `poem` en lugar de axum (ver [`AXUM.md`](AXUM.md#alternativas-consideradas)). |
| **paperclip / okapi** | Equivalentes para actix-web y Rocket. | Atados a frameworks que no usamos y con menos actividad. |

---

## Cómo lo usamos

### Rutas

```rust
#[utoipa::path(
    post,
    path = "/v1/auctions/{auction_id}/bids",
    tag = "pujas",
    params(("auction_id" = Uuid, Path), ("x-client-channel" = Option<String>, Header, …)),
    request_body = BidBody,
    responses(
        (status = 200, body = DecisionResponse),
        (status = 401, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "La clave idempotente ya se usó con otro pedido.", body = ErrorBody),
    ),
    security(("bearer" = []))
)]
pub async fn place_bid(…) -> Result<Json<DecisionResponse>, ApiError> { … }
```

En `api_router`, `OpenApiRouter::with_openapi(ApiDoc::openapi()).routes(routes!(…))` registra
la ruta en axum y en el documento a la vez. Dos handlers con el mismo path y distinto método
(`GET` y `POST /bids`) van en el mismo `routes!`.

### Documento base

`ApiDoc` (`#[derive(OpenApi)]` en `lib.rs`) aporta título, versión, descripción y los *tags*
(subastas, pujas, tiempo real, administración, operación). `SecurityAddon` registra el
esquema `bearer` (JWT). También se registran a mano dos esquemas que ningún handler REST
devuelve: `ServerMessage`, el mensaje del WebSocket, y `Role`.

### Tipos

- **Cuerpos y respuestas** de la API (`BidBody`, `DecisionResponse`, `ErrorBody`…) derivan
  `ToSchema` directamente.
- **Tipos del dominio** (`Amount`, `Currency`, `Rules`, `Rejection`…) y del store
  (`PublicStanding`, `Decision`…) lo derivan con la feature `openapi`.
- **Tipos con serialización propia** declaran explícitamente cómo se ven en JSON. `Amount`
  se serializa con `try_from`/`into` de `i64`, así que declara
  `schema(value_type = i64, example = 50000)`: en el contrato es un `integer`/`int64` con
  ejemplo, igual que en el cable.
- Los comentarios `///` de los campos pasan a ser la descripción en el documento.

### El WebSocket en el contrato

OpenAPI no describe protocolos de mensajes. `GET /v1/auctions/{auction_id}/ws` figura como
ruta, con su autenticación, y el formato de los mensajes se publica en el esquema
`ServerMessage`. El protocolo completo (snapshot, `version`, `resync`) está en el comentario
del módulo `routes/ws.rs`. Si hiciera falta un contrato formal del socket, la herramienta
indicada es AsyncAPI.

### Cómo se verifica

| Verificación | Qué asegura |
|---|---|
| CI: `cargo run -q -p remates-api -- openapi \| diff -u docs/openapi.json -` | El contrato versionado es exactamente el que genera el código. Después de cambiar la API hay que regenerarlo (ver README). |
| Prueba `openapi_is_served_and_complete` | Las rutas principales están, toda `$ref` apunta a un esquema que existe y `/docs` responde. |

---

## Costos y riesgos

| Riesgo | Mitigación |
|---|---|
| **Las respuestas se declaran a mano.** utoipa toma el cuerpo y los parámetros de la anotación, no del tipo de retorno. Un handler puede devolver un 422 que su `responses(...)` no menciona. | Al revisar el código, comparar `responses(...)` con lo que puede devolver `ApiError`. Hoy, por ejemplo, ninguna ruta declara el 408 del timeout, y las de pujas no declaran el 400/422 que devuelve axum si el JSON no es válido. |
| **Los `params(...)` también se declaran a mano**, así que pueden quedar desfasados con los extractores. | La prueba de completitud no detecta esto; queda para la revisión. Opción: `IntoParams` sobre structs de query y path. |
| **Macros pesadas.** Suman tiempo de compilación y, si fallan, dan errores poco claros. | Aceptable para el tamaño actual. |
| **`/docs` carga Scalar desde un CDN** (`cdn.jsdelivr.net`). Sin internet o con una CSP estricta, la página no se ve. `/openapi.json` sigue funcionando. | Para producción: servir Scalar o Swagger UI desde un recurso propio, o no exponer `/docs`. |

## Cuándo reconsiderarlo

- Si el contrato se volviera *spec-first*, porque lo define otro equipo o un cliente externo.
  Ahí conviene generar los tipos desde el documento y usar el chequeo de CI en la otra
  dirección.
- Si el WebSocket necesitara un contrato formal: sumar AsyncAPI al lado de OpenAPI, no
  reemplazarlo.
