# Paraguay Remates Engine

Backend de la plataforma de subastas agrícolas de Paraguay Remates.

- **Qué construimos y por qué:** [`SPEC.md`](SPEC.md) y [`docs/CONVERSATION.md`](docs/CONVERSATION.md).
- **Fuente de verdad funcional:** [especificación técnica v3](docs/Paraguay_Remates_Especificacion_Tecnica_Revisada_v3.pdf).
- **Arquitectura, escalado y backups:** [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

## Estado

| Parte (SPEC.md) | Estado |
|---|---|
| 4 — Motor de subastas | Hecho: martillo y reserva, pujas manuales y máximos automáticos, alargue, cierre, certificado secuencial. Las reglas pendientes P-04 son configurables. |
| 1 — Plataforma base | Hecho en parte: PostgreSQL, migraciones, pools primario/réplica, outbox, CI. |
| Resto | Pendiente. |

## Requisitos

- Rust stable (ver `rust-toolchain.toml`)
- PostgreSQL 16 (o Docker)

## Desarrollo

Levantar Postgres:

```bash
docker compose up -d db
```

Variables de entorno:

```bash
export DATABASE_URL=postgres://postgres:postgres@localhost:5432/remates
```

Correr todas las pruebas. Las del store crean una base temporal por prueba:

```bash
cargo test
```

Lint y formato:

```bash
cargo clippy --all-targets -- -D warnings && cargo fmt --all --check
```

| Variable | Uso |
|---|---|
| `DATABASE_URL` | Primaria: escrituras y lecturas que deciden algo. |
| `DATABASE_REPLICA_URL` | Opcional. Réplica de lectura para vistas públicas. |
| `DATABASE_MAX_CONNECTIONS` | Tamaño de cada pool (por defecto 20). |
