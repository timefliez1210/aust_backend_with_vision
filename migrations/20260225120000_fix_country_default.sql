-- Fix: default country should be Deutschland, not Österreich
ALTER TABLE addresses ALTER COLUMN country SET DEFAULT 'Deutschland';
