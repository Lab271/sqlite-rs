CREATE TABLE IF NOT EXISTS iceberg_tables (
  catalog_name TEXT,
  table_namespace TEXT,
  table_name TEXT,
  metadata_location TEXT,
  PRIMARY KEY (catalog_name, table_namespace, table_name)
);
INSERT INTO iceberg_tables VALUES ('cat','ns','t1','s3://bucket/t1/meta.json');
INSERT INTO iceberg_tables VALUES ('cat','ns','t2','s3://bucket/t2/meta.json');
