-- Add CHECK constraint to accent_color: only hex colors (#RGB or #RRGGBB) are allowed.
-- This prevents CSS injection in contexts where the value appears in a style attribute.
-- The existing migration (20260713000000) added the column without a constraint.
ALTER TABLE restaurants
    ADD CONSTRAINT restaurants_accent_color_check
    CHECK (accent_color IS NULL OR accent_color ~ '^#[0-9a-fA-F]{3}([0-9a-fA-F]{3})?$');
