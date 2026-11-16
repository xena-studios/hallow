-- Fleet manager settings (docs/FLEET.md): everything the fleet manager
-- decides by, edited from the panel. One row holds the settings in force as
-- JSON (`hallow_proto::fleet::FleetSettings`); without a row the defaults
-- apply. Every change also goes in the append-only history.
CREATE TABLE hallow.fleet_settings (
    -- Always 1: there is one set of settings per network.
    id         smallint    PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    settings   jsonb       NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    updated_by text        NOT NULL CHECK (char_length(updated_by) BETWEEN 1 AND 64)
);

-- Every change to `fleet_settings`, never updated or deleted. `before` is
-- NULL for the first change, made over the defaults.
CREATE TABLE hallow.fleet_settings_history (
    id         bigserial   PRIMARY KEY,
    before     jsonb,
    after      jsonb       NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    updated_by text        NOT NULL CHECK (char_length(updated_by) BETWEEN 1 AND 64)
);

CREATE INDEX fleet_settings_history_updated_at_idx
    ON hallow.fleet_settings_history (updated_at DESC, id DESC);
