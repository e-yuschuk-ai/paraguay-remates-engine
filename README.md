# Paraguay Remates Engine

Backend de la plataforma de subastas agrícolas de Paraguay Remates.

- **Qué construimos y por qué:** [`SPEC.md`](SPEC.md) y [`docs/CONVERSATION.md`](docs/CONVERSATION.md).
- **Fuente de verdad funcional:** [especificación técnica v3](docs/Paraguay_Remates_Especificacion_Tecnica_Revisada_v3.pdf).
- **Arquitectura, escalado y backups:** [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).
- **Contrato de la API:** [`docs/openapi.json`](docs/openapi.json). Con el servidor corriendo: `/docs`.

## Estado

| Parte (SPEC.md) | Estado |
|---|---|
| 4 — Motor de subastas | Hecho: martillo y reserva, pujas manuales y máximos automáticos, alargue, cierre y certificado secuencial. Las reglas pendientes P-04 son configurables. |
| 5 — Tiempo real | Hecho: WebSocket por subasta con snapshot, eventos versionados y `resync`. El polling de respaldo es `GET /v1/auctions/{id}`. |
| 6 — Notificaciones web | Hecho en parte: los avisos privados (oferta superada, decisión, adjudicación) llegan por el WebSocket. Falta la interfaz. |
| 1 — Plataforma base | Hecho en parte: PostgreSQL, migraciones, pools primario/réplica, outbox, API con OpenAPI y CI. |
| Resto | Pendiente. |

## Crates

| Crate | Qué es |
|---|---|
| `remates-engine` | Dominio puro del motor. Sin IO. |
| `remates-store` | PostgreSQL: transacciones, idempotencia, cierres y outbox. |
| `remates-api` | Servidor HTTP + WebSocket (axum), contrato OpenAPI (utoipa). |

## Requisitos

- Rust stable (ver `rust-toolchain.toml`)
- PostgreSQL 16 (o Docker)

## Desarrollo

Levantar Postgres:

```bash
docker compose up -d db
```

Copiar la configuración de ejemplo y cargarla:

```bash
cp .env.example .env && set -a && . ./.env && set +a
```

Arrancar el servidor (con `RUN_MIGRATIONS=true` aplica las migraciones):

```bash
cargo run -p remates-api
```

Correr todas las pruebas. Las del store y la API crean una base temporal por prueba:

```bash
cargo test
```

Lint y formato:

```bash
cargo clippy --all-targets -- -D warnings && cargo fmt --all --check
```

Regenerar el contrato OpenAPI después de cambiar la API (la CI verifica que esté al día):

```bash
cargo run -q -p remates-api -- openapi > docs/openapi.json
```

## Configuración

| Variable | Uso |
|---|---|
| `DATABASE_URL` | Primaria: escrituras y lecturas que deciden algo. |
| `DATABASE_REPLICA_URL` | Opcional. Réplica de lectura para vistas públicas y difusión. |
| `DATABASE_MAX_CONNECTIONS` | Tamaño de cada pool (por defecto 20). |
| `BIND_ADDR` | Dirección del servidor (por defecto `0.0.0.0:8080`). |
| `JWT_SECRET` | Secreto HS256, mínimo 32 bytes. Obligatorio. |
| `JWT_ISSUER` | Emisor esperado en los tokens (por defecto `paraguay-remates`). |
| `RUN_MIGRATIONS` | `true` para aplicar migraciones al arrancar. |
| `RUN_WORKERS` | `true` (por defecto) para correr el cierre automático en esta instancia. |
| `CORS_ALLOWED_ORIGINS` | Orígenes permitidos, separados por coma. Vacío: sin CORS. |
| `LOG_FORMAT` | `json` para logs estructurados. |
| `RUST_LOG` | Filtro de logs, por ejemplo `info,tower_http=debug`. |

## API en una mirada

| Método y ruta | Quién | Qué hace |
|---|---|---|
| `POST /v1/auctions` | vendedor o admin | Publica una subasta. |
| `GET /v1/auctions/{id}` | público | Posición: precio, alias del líder, próxima oferta, cierre. |
| `GET /v1/auctions/{id}/bids` | público | Historial con alias. |
| `POST /v1/auctions/{id}/bids` | postor | Puja manual. |
| `POST /v1/auctions/{id}/max-bids` | postor | Puja máxima automática. |
| `GET /v1/auctions/{id}/ws` | público; con token, también avisos privados | WebSocket de la subasta. |
| `GET /v1/admin/auctions/{id}/bids` | admin | Historial con identidades reales. |
| `POST /v1/admin/auctions/{id}/close` | admin | Cierra ya si llegó la hora. |
| `GET /health`, `GET /ready` | orquestador | Sondas. |

Reglas para los clientes:

- **Importes** en unidades menores: centavos para USD, guaraníes para PYG.
- **Cada intento de puja lleva un `request_id`** (UUID) generado por el cliente. Si la respuesta
  no llega, se reintenta con el mismo `request_id`: se obtiene la misma decisión y la puja no
  se duplica.
- **Una puja rechazada responde 200** con `decision.status = "rejected"` y el motivo; por
  ejemplo `below_minimum` incluye el mínimo vigente. Los errores HTTP (401, 403, 404, 409)
  son problemas del pedido, no de la subasta.
- **WebSocket:** al conectar llega un `snapshot`; después, eventos con `version`. El cliente
  aplica sólo versiones mayores a la que tiene y, si ve un salto o reconecta, manda `resync`.
