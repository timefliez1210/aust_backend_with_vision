-- Multi-tenancy, step 2: the company's own words live in data, not in code.
--
-- Additive only. Aust's values are exactly the strings the code used until now, so
-- every mail, subject and document stays byte-identical. See docs/MULTI_TENANT.md.
--
-- `tenants.name` is the full company name ("Aust Umzüge & Haushaltsauflösungen").

ALTER TABLE tenants
    ADD COLUMN short_name  TEXT NOT NULL DEFAULT '',  -- OTP mails, short signatures
    ADD COLUMN brand_name  TEXT NOT NULL DEFAULT '',  -- offer mails, auto-replies
    ADD COLUMN owner_name  TEXT NOT NULL DEFAULT '',  -- supervisor on timesheets
    ADD COLUMN phone       TEXT NOT NULL DEFAULT '',  -- as printed in customer mails
    ADD COLUMN city        TEXT NOT NULL DEFAULT '',  -- "ein Umzugsunternehmen in …"
    ADD COLUMN review_url  TEXT NOT NULL DEFAULT '',  -- Google review link
    ADD COLUMN depot_address TEXT NOT NULL DEFAULT ''; -- start and end of every route (Fahrkostenpauschale, map)

UPDATE tenants SET
    short_name = 'Aust Umzüge',
    brand_name = 'AUST Umzüge',
    owner_name = 'Alex Aust',
    phone      = '05121 – 7558379',
    city       = 'Hildesheim',
    review_url = 'https://www.google.com/search?q=Aust+Umz%C3%BCge+%26+Haushaltsaufl%C3%B6sungen+Reviews',
    depot_address = 'Borsigstr 6 31135 Hildesheim'
WHERE id = '0190aa57-0000-7000-8000-000000000001';
