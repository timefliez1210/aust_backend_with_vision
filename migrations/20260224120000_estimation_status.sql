-- Add status column to volume_estimations for async video processing
ALTER TABLE volume_estimations ADD COLUMN status VARCHAR(50) NOT NULL DEFAULT 'completed';

-- Existing rows are already completed
-- New video estimates will start as 'processing' and update to 'completed' or 'failed'
