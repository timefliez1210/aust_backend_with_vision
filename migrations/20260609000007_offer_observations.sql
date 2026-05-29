-- Offer adjustment observations for the offline learning pipeline (Phase 5).
--
-- Every time the assistant proposes an offer and Alex edits it, the difference is
-- recorded here.  Over time this dataset trains the LinfaPredictor to anticipate
-- Alex's adjustments before he has to make them.
--
-- `features`  — extracted from Inquiry + Offer at proposal time (OfferFeatures JSON).
-- `proposed`  — the offer as the pricing engine generated it (key figures in cents).
-- `final`     — the offer after Alex's edits.
-- `edit_distance` — structured diff between proposed and final (e.g. {"price_delta": 5000}).

CREATE TABLE offer_observations (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    inquiry_id      UUID NOT NULL REFERENCES inquiries(id),
    offer_id        UUID NOT NULL REFERENCES offers(id),
    features        JSONB NOT NULL,
    proposed        JSONB NOT NULL,
    final           JSONB,
    edit_distance   JSONB,
    -- Was this observation used in the most recent model training run?
    used_in_training BOOLEAN NOT NULL DEFAULT FALSE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_offer_observations_inquiry_id       ON offer_observations(inquiry_id);
CREATE INDEX idx_offer_observations_used_in_training ON offer_observations(used_in_training)
    WHERE NOT used_in_training;
CREATE INDEX idx_offer_observations_created_at       ON offer_observations(created_at DESC);
