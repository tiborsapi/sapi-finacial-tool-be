CREATE TABLE IF NOT EXISTS expenses (
    id UUID PRIMARY KEY,
    amount_cents BIGINT NOT NULL CHECK (amount_cents > 0),
    source VARCHAR(120) NOT NULL,
    category VARCHAR(32) NOT NULL,
    description VARCHAR(500),
    received_on DATE NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS expenses_received_on_idx ON expenses (received_on DESC);
CREATE INDEX IF NOT EXISTS expenses_category_idx ON expenses (category);
