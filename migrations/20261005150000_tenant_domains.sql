-- Multi-tenancy, step 6: which company a public request is for, and its colour.
--
-- Additive only. `domains` are the hosts a company's website and console are
-- served from: a form posted from one of them (Origin header) creates its
-- inquiry in that company, the API accepts cross-origin calls from them (CORS),
-- and `GET /api/v1/tenant` answers with that company's branding. A request from
-- no listed domain stays Aust's, as today. `accent_color` is the console's
-- accent (Aust: the current orange).

ALTER TABLE tenants
    ADD COLUMN domains      TEXT[] NOT NULL DEFAULT '{}',
    ADD COLUMN accent_color TEXT   NOT NULL DEFAULT '#ff5a1f'
        CHECK (accent_color ~ '^#[0-9a-fA-F]{6}$');

UPDATE tenants
SET domains = ARRAY['aust-umzuege.de', 'www.aust-umzuege.de']
WHERE id = '0190aa57-0000-7000-8000-000000000001';
