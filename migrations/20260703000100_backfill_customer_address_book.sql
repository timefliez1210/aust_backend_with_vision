-- Backfill the customer address book from every address a customer's inquiries
-- have ever referenced (origin/destination/stop/billing), plus the customer's
-- own billing address.
--
-- Deduped via the uniq_customer_address index (ON CONFLICT DO NOTHING).
-- DISTINCT ON keeps one representative row per (customer, normalized address),
-- preferring the most recently created so intra-batch duplicates can't collide.
-- source = 'inquiry' since these all originate from historical structured data.

INSERT INTO customer_addresses (
    id, customer_id, street, house_number, postal_code, city, country,
    floor, elevator, parking_ban, latitude, longitude, source,
    last_used_at, created_at
)
SELECT DISTINCT ON (
        pair.customer_id,
        lower(a.street),
        coalesce(a.house_number, ''),
        coalesce(a.postal_code, ''),
        lower(a.city)
    )
    gen_random_uuid(),
    pair.customer_id,
    a.street,
    a.house_number,
    a.postal_code,
    a.city,
    coalesce(a.country, 'Deutschland'),
    a.floor,
    a.elevator,
    coalesce(a.parking_ban, false),
    a.latitude,
    a.longitude,
    'inquiry',
    a.created_at,
    a.created_at
FROM (
    -- All (customer, address) pairs reachable from inquiries.
    SELECT customer_id, origin_address_id      AS address_id FROM inquiries WHERE origin_address_id      IS NOT NULL
    UNION ALL
    SELECT customer_id, destination_address_id AS address_id FROM inquiries WHERE destination_address_id IS NOT NULL
    UNION ALL
    SELECT customer_id, stop_address_id        AS address_id FROM inquiries WHERE stop_address_id         IS NOT NULL
    UNION ALL
    SELECT customer_id, billing_address_id     AS address_id FROM inquiries WHERE billing_address_id      IS NOT NULL
    UNION ALL
    -- The customer's own billing address (covers customers without inquiries).
    SELECT id AS customer_id, billing_address_id AS address_id FROM customers WHERE billing_address_id    IS NOT NULL
) AS pair
JOIN addresses a ON a.id = pair.address_id
WHERE a.street IS NOT NULL AND a.street <> ''
ORDER BY
    pair.customer_id,
    lower(a.street),
    coalesce(a.house_number, ''),
    coalesce(a.postal_code, ''),
    lower(a.city),
    a.created_at DESC
ON CONFLICT DO NOTHING;
