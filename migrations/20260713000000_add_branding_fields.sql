-- Migration for Milestone 6 - Branding Fields
-- Added accent_color and logo_url to restaurants table (M6 slice 1)

ALTER TABLE restaurants ADD COLUMN IF NOT EXISTS accent_color VARCHAR(50);
ALTER TABLE restaurants ADD COLUMN IF NOT EXISTS logo_url TEXT;

-- Add constraints or indexes if required by future logic, but leaving fields optional for now.
