-- Games can be turned off from the panel (#32). A disabled kind starts no new
-- instances and takes no new queue entries; instances already running finish
-- their games. The column is the truth: specs are read with it.
ALTER TABLE hallow.server_kinds ADD COLUMN enabled boolean NOT NULL DEFAULT true;
