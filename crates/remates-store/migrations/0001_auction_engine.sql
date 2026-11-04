-- Motor de subastas: esquema inicial.
-- Importes en unidades menores (bigint): centavos para USD, guaraníes para PYG (RT-04).

-- Usuarios: versión mínima para que el motor verifique habilitación (RF-10).
-- La Parte 2 (identidad y roles) la va a extender.
CREATE TABLE users (
    id          uuid PRIMARY KEY,
    status      text NOT NULL DEFAULT 'active'
                CHECK (status IN ('active', 'suspended', 'removed')),
    -- Identidad verificada + 2FA vigente: puede pujar.
    can_bid     boolean NOT NULL DEFAULT false,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE auctions (
    id                    uuid PRIMARY KEY,
    seller_id             uuid NOT NULL REFERENCES users (id),
    currency              text NOT NULL CHECK (currency IN ('USD', 'PYG')),
    kind                  text NOT NULL CHECK (kind IN ('hammer', 'reserve')),
    base_price            bigint CHECK (base_price > 0),
    increment             bigint NOT NULL CHECK (increment > 0),
    first_bid_rule        text NOT NULL CHECK (first_bid_rule IN ('increment', 'any_positive')),
    opens_at              timestamptz NOT NULL,
    scheduled_close_at    timestamptz NOT NULL,
    -- Cierre vigente, con alargues aplicados.
    closes_at             timestamptz NOT NULL,
    extension_window_ms   bigint NOT NULL CHECK (extension_window_ms >= 0),
    extension_ms          bigint NOT NULL CHECK (extension_ms >= 0),
    extension_mode        text NOT NULL CHECK (extension_mode IN ('reset_from_bid', 'add_to_close')),
    status                text NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'closed')),
    outcome               text CHECK (outcome IN ('awarded', 'no_sale')),
    leader_id             uuid REFERENCES users (id),
    price                 bigint CHECK (price > 0),
    -- Último orden del servidor asignado (RF-14). Compartido por bids y max_bids.
    last_seq              bigint NOT NULL DEFAULT 0,
    -- Versión del estado, incluida en cada evento (RT-06).
    version               bigint NOT NULL DEFAULT 0,
    closed_at             timestamptz,
    created_at            timestamptz NOT NULL DEFAULT now(),
    updated_at            timestamptz NOT NULL DEFAULT now(),
    CHECK ((kind = 'reserve') = (base_price IS NOT NULL)),
    CHECK (scheduled_close_at > opens_at),
    CHECK (closes_at >= scheduled_close_at),
    CHECK ((leader_id IS NULL) = (price IS NULL)),
    CHECK ((status = 'closed') = (outcome IS NOT NULL)),
    CHECK ((status = 'closed') = (closed_at IS NOT NULL)),
    CHECK (outcome IS DISTINCT FROM 'awarded' OR leader_id IS NOT NULL)
);

-- Cola de cierres: sólo subastas activas, por hora de cierre.
CREATE INDEX auctions_due_idx ON auctions (closes_at) WHERE status = 'active';
CREATE INDEX auctions_seller_idx ON auctions (seller_id);

-- Posición de cada postor en una subasta. `max_amount` es privado (§5.5).
CREATE TABLE auction_bidders (
    auction_id    uuid NOT NULL REFERENCES auctions (id),
    bidder_id     uuid NOT NULL REFERENCES users (id),
    alias         integer NOT NULL CHECK (alias > 0),
    max_amount    bigint NOT NULL CHECK (max_amount > 0),
    floor_amount  bigint CHECK (floor_amount > 0),
    shown_amount  bigint CHECK (shown_amount > 0),
    priority_seq  bigint NOT NULL CHECK (priority_seq > 0),
    has_proxy     boolean NOT NULL,
    PRIMARY KEY (auction_id, bidder_id),
    UNIQUE (auction_id, alias),
    CHECK (floor_amount IS NULL OR floor_amount <= max_amount),
    CHECK (shown_amount IS NULL OR shown_amount <= max_amount)
);
CREATE INDEX auction_bidders_bidder_idx ON auction_bidders (bidder_id);

-- Historial público de pujas (RF-13). Append-only.
CREATE TABLE bids (
    auction_id   uuid NOT NULL REFERENCES auctions (id),
    seq          bigint NOT NULL CHECK (seq > 0),
    bidder_id    uuid NOT NULL REFERENCES users (id),
    amount       bigint NOT NULL CHECK (amount > 0),
    kind         text NOT NULL CHECK (kind IN ('manual', 'auto')),
    -- Canal del pedido que originó la línea (las automáticas heredan el del disparador).
    source       text NOT NULL CHECK (source IN ('web', 'app', 'whatsapp', 'admin')),
    request_id   uuid NOT NULL,
    created_at   timestamptz NOT NULL,
    PRIMARY KEY (auction_id, seq)
);

-- Registro privado de máximos automáticos (RF-11). Append-only.
CREATE TABLE max_bids (
    auction_id   uuid NOT NULL REFERENCES auctions (id),
    seq          bigint NOT NULL CHECK (seq > 0),
    bidder_id    uuid NOT NULL REFERENCES users (id),
    max_amount   bigint NOT NULL CHECK (max_amount > 0),
    source       text NOT NULL CHECK (source IN ('web', 'app', 'whatsapp', 'admin')),
    request_id   uuid NOT NULL,
    created_at   timestamptz NOT NULL,
    PRIMARY KEY (auction_id, seq)
);

-- Todo pedido de puja y su decisión, aceptada o rechazada (RT-02).
-- La clave idempotente es del cliente, por eso se acota al postor.
CREATE TABLE bid_requests (
    bidder_id    uuid NOT NULL,
    request_id   uuid NOT NULL,
    auction_id   uuid NOT NULL REFERENCES auctions (id),
    action       text NOT NULL CHECK (action IN ('bid', 'set_max')),
    amount       bigint NOT NULL,
    currency     text NOT NULL,
    source       text NOT NULL CHECK (source IN ('web', 'app', 'whatsapp', 'admin')),
    accepted     boolean NOT NULL,
    seq          bigint,
    decision     jsonb NOT NULL,
    received_at  timestamptz NOT NULL,
    PRIMARY KEY (bidder_id, request_id),
    CHECK (accepted = (seq IS NOT NULL))
);
CREATE INDEX bid_requests_auction_idx ON bid_requests (auction_id, received_at);

-- Una adjudicación como máximo por subasta (RT-07).
CREATE TABLE awards (
    auction_id          uuid PRIMARY KEY REFERENCES auctions (id),
    winner_id           uuid NOT NULL REFERENCES users (id),
    amount              bigint NOT NULL CHECK (amount > 0),
    currency            text NOT NULL,
    -- Número del certificado PDF: N001, N002… (RF-26). Sin huecos.
    certificate_number  bigint NOT NULL UNIQUE CHECK (certificate_number > 0),
    awarded_at          timestamptz NOT NULL
);

-- Contadores sin huecos (una secuencia de Postgres puede saltear números).
CREATE TABLE counters (
    name   text PRIMARY KEY,
    value  bigint NOT NULL
);
INSERT INTO counters (name, value) VALUES ('award_certificate', 0);

-- Outbox transaccional (§5.1): los eventos se guardan en la misma transacción
-- que el cambio y un proceso aparte los publica.
CREATE TABLE outbox (
    id                 bigserial PRIMARY KEY,
    aggregate_id       uuid NOT NULL,
    aggregate_version  bigint NOT NULL,
    event_type         text NOT NULL,
    audience           text NOT NULL CHECK (audience IN ('public', 'user', 'admin')),
    recipient_id       uuid,
    payload            jsonb NOT NULL,
    created_at         timestamptz NOT NULL DEFAULT now(),
    published_at       timestamptz,
    CHECK ((audience = 'user') = (recipient_id IS NOT NULL))
);
CREATE INDEX outbox_pending_idx ON outbox (id) WHERE published_at IS NULL;

-- Despierta a los publicadores sin tener que consultar en bucle.
CREATE FUNCTION outbox_notify() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    PERFORM pg_notify('outbox', '');
    RETURN NULL;
END;
$$;
CREATE TRIGGER outbox_notify AFTER INSERT ON outbox
    FOR EACH STATEMENT EXECUTE FUNCTION outbox_notify();

-- Historial y registros append-only: se bloquea UPDATE y DELETE.
CREATE FUNCTION forbid_mutation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION '% es append-only', TG_TABLE_NAME;
END;
$$;
CREATE TRIGGER bids_append_only BEFORE UPDATE OR DELETE ON bids
    FOR EACH ROW EXECUTE FUNCTION forbid_mutation();
CREATE TRIGGER max_bids_append_only BEFORE UPDATE OR DELETE ON max_bids
    FOR EACH ROW EXECUTE FUNCTION forbid_mutation();
CREATE TRIGGER bid_requests_append_only BEFORE UPDATE OR DELETE ON bid_requests
    FOR EACH ROW EXECUTE FUNCTION forbid_mutation();
CREATE TRIGGER awards_append_only BEFORE UPDATE OR DELETE ON awards
    FOR EACH ROW EXECUTE FUNCTION forbid_mutation();
