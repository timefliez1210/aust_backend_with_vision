# Register-Import

Alex's Excel Rechnungsausgangsbuch is the authoritative record for the years he
invoiced by hand. This directory holds the tooling that merges it into production
without ever writing to the database straight from the spreadsheet.

## Ablauf

1. **Dump production** (read-only):

   ```bash
   SSH="ssh root@187.124.161.90 docker exec aust_postgres psql -U aust -d aust_backend -tAc"

   $SSH "SELECT inv.invoice_number,coalesce(c.name,''),inv.invoice_type,
                coalesce(inv.partial_percent::text,''),inv.status,
                coalesce(inv.base_netto_cents::text,''),
                coalesce((SELECT o.price_cents::text FROM offers o
                          WHERE o.inquiry_id=inv.inquiry_id
                          ORDER BY o.created_at DESC LIMIT 1),''),
                CASE WHEN inv.sent_at IS NULL THEN 'nein' ELSE 'ja' END
         FROM invoices inv
         LEFT JOIN inquiries i ON i.id=inv.inquiry_id
         LEFT JOIN customers c ON c.id=COALESCE(inv.customer_id,i.customer_id)
         WHERE inv.invoice_number ~ '^2026-[0-9]+$'
         ORDER BY (split_part(inv.invoice_number,'-',2))::int" > prod_invoices.txt

   $SSH "SELECT id||'|'||name FROM customers ORDER BY name" > prod_customers.txt
   ```

2. **Build the review workbook** and send it to Alex:

   ```bash
   python3 build_review_workbook.py \
     --excel "Rechnungsausgangsbuch 2024.xlsx" --sheet 2026 \
     --prod-invoices prod_invoices.txt --prod-customers prod_customers.txt \
     --out Abgleich_2026.xlsx
   ```

3. **He reviews it.** Every row already carries a decision; he only touches the
   ones coloured yellow or red. The dropdown constrains him to four answers, so
   nothing comes back that the importer cannot parse.

4. **Dry-run the import against a restored backup**, never against production.

5. **Run it for real** inside a transaction, right after a fresh backup.

## Warum kein CSV

Alex works in Excel. Colour does the triage for him — of 86 rows only ~32 need a
human, and he can see which at a glance. The dropdowns stop a fifth answer from
appearing. A CSV would have handed him the triage as homework.

## Warum keine Migration

The import is one-time production data, not schema. A migration would re-run on
every environment, including fresh test databases, and would bake one company's
2026 invoices into the schema history. The importer is a separate, idempotent,
`--dry-run`-able step.
