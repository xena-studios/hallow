-- Friends: requests waiting for an answer, and friendships.

-- A request from one account to another. Answering it (accept or deny)
-- deletes it; a request the other way round is accepted instead of stored.
CREATE TABLE hallow.friend_requests (
    from_uuid   uuid        NOT NULL REFERENCES hallow.accounts (uuid) ON DELETE CASCADE,
    to_uuid     uuid        NOT NULL REFERENCES hallow.accounts (uuid) ON DELETE CASCADE,
    created_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (from_uuid, to_uuid),
    CHECK (from_uuid <> to_uuid)
);

-- Requests waiting for an account, for its pending list.
CREATE INDEX friend_requests_to_idx ON hallow.friend_requests (to_uuid, created_at);

-- One row per friendship, stored once with the smaller UUID first.
CREATE TABLE hallow.friendships (
    a_uuid  uuid        NOT NULL REFERENCES hallow.accounts (uuid) ON DELETE CASCADE,
    b_uuid  uuid        NOT NULL REFERENCES hallow.accounts (uuid) ON DELETE CASCADE,
    since   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (a_uuid, b_uuid),
    CHECK (a_uuid < b_uuid)
);

-- The primary key covers lookups by a_uuid; this covers b_uuid.
CREATE INDEX friendships_b_idx ON hallow.friendships (b_uuid);
