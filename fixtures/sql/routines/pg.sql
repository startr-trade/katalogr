CREATE FUNCTION app.order_count(cid bigint) RETURNS bigint
LANGUAGE sql AS $$ SELECT count(*) FROM orders WHERE customer_id = cid $$;

CREATE FUNCTION app.order_count(cid bigint, since date) RETURNS bigint
LANGUAGE sql AS $$ SELECT count(*) FROM orders WHERE customer_id = cid $$;

CREATE OR REPLACE FUNCTION app.touch() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  UPDATE audit SET touched = now() WHERE id = NEW.id;
  PERFORM app.recalc();
  RETURN NEW;
END;
$$;

CREATE PROCEDURE purge_old(days int)
LANGUAGE sql AS $$ DELETE FROM orders WHERE placed_at < now() - days * interval '1 day' $$;
