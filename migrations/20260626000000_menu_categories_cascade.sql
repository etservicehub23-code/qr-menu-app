-- Add ON DELETE CASCADE to menu_categories.restaurant_id FK
ALTER TABLE menu_categories
    DROP CONSTRAINT menu_categories_restaurant_id_fkey,
    ADD CONSTRAINT menu_categories_restaurant_id_fkey
        FOREIGN KEY (restaurant_id) REFERENCES restaurants(id) ON DELETE CASCADE;
