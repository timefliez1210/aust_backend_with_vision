-- Prevent duplicate active bookings per quote
CREATE UNIQUE INDEX idx_calendar_bookings_quote_active
  ON calendar_bookings(quote_id)
  WHERE quote_id IS NOT NULL AND status != 'cancelled';
