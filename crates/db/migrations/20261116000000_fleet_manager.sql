-- The fleet manager's nodes and its audit log (docs/FLEET.md).
--
-- Every machine the fleet manager knows: the floor nodes from its fleet file
-- and every node it bought, dry run included. A cancelled node keeps its
-- row, so this month's spend can be worked out from the table alone.
CREATE TABLE hallow.fleet_nodes (
    -- Also the node's name in the orchestrator and the panel.
    name               text        PRIMARY KEY CHECK (char_length(name) BETWEEN 1 AND 64),
    provider           text        NOT NULL,
    -- The provider's id; NULL for a floor node from the fleet file.
    provider_id        text,
    kind               text        NOT NULL CHECK (kind IN ('base', 'burst')),
    region             text        NOT NULL,
    machine_type       text        NOT NULL,
    slots              integer     NOT NULL CHECK (slots >= 0),
    -- The price per billing step in millionths of the currency.
    cost_micros        bigint      NOT NULL CHECK (cost_micros >= 0),
    billing_step       text        NOT NULL CHECK (billing_step IN ('hour', 'month')),
    monthly_cap_micros bigint      CHECK (monthly_cap_micros >= 0),
    phase              text        NOT NULL
                       CHECK (phase IN ('setting_up', 'ready', 'draining', 'cancelled')),
    -- Bought in dry run: no machine exists, and it is cancelled through dry run.
    dry_run            boolean     NOT NULL,
    -- From the fleet file's [[floor]]: never drained or cancelled.
    floor              boolean     NOT NULL DEFAULT false,
    created_at         timestamptz NOT NULL,
    ready_at           timestamptz,
    draining_since     timestamptz,
    cancelled_at       timestamptz,
    -- A base node waiting for staff to approve its cancellation.
    awaiting_approval  boolean     NOT NULL DEFAULT false,
    approved_by        text        CHECK (char_length(approved_by) BETWEEN 1 AND 64)
);

CREATE INDEX fleet_nodes_live_idx ON hallow.fleet_nodes (phase) WHERE phase <> 'cancelled';

-- Every decision, provider call, staff action and alert, never updated.
CREATE TABLE hallow.fleet_events (
    id      bigserial   PRIMARY KEY,
    at      timestamptz NOT NULL,
    kind    text        NOT NULL
            CHECK (kind IN ('decision', 'provider', 'node', 'proxy', 'staff', 'alert')),
    action  text        NOT NULL,
    node    text,
    dry_run boolean     NOT NULL,
    ok      boolean     NOT NULL DEFAULT true,
    message text        NOT NULL,
    by      text,
    detail  jsonb
);

CREATE INDEX fleet_events_at_idx ON hallow.fleet_events (id DESC);
CREATE INDEX fleet_events_kind_idx ON hallow.fleet_events (kind, id DESC);
