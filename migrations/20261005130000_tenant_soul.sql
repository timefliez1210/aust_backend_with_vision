-- Multi-tenancy, step 4: the assistant's persona per company.
--
-- Additive only. NULL = Aust keeps SOUL.md (the file bundled with the backend);
-- any other company without its own gets a neutral persona under its own name.
-- Same format as SOUL.md (# Persona, # Hard Rules, # Domain Primer, # Tone,
-- # Escalation).

ALTER TABLE tenants ADD COLUMN soul_md TEXT;
