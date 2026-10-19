CREATE PROCEDURE archive_orders(cutoff date)
LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO audit (note) SELECT id::text FROM orders WHERE placed_at < cutoff;
  DELETE FROM orders WHERE placed_at < cutoff;
  CALL app.vacuum_orders();
END;
$$;

DO $$
BEGIN
  UPDATE customers SET email = lower(email);
END;
$$;
