CREATE TABLE IF NOT EXISTS iceberg_tables (
  catalog_name TEXT,
  table_namespace TEXT,
  table_name TEXT,
  metadata_location TEXT
);
CREATE UNIQUE INDEX IF NOT EXISTS iceberg_tables_pk
  ON iceberg_tables (catalog_name, table_namespace, table_name);
CREATE TABLE IF NOT EXISTS iceberg_namespace_properties (
  catalog_name TEXT,
  namespace TEXT,
  property_key TEXT,
  property_value TEXT
);
CREATE UNIQUE INDEX IF NOT EXISTS iceberg_namespace_properties_pk
  ON iceberg_namespace_properties (catalog_name, namespace, property_key);
INSERT INTO iceberg_tables VALUES ('cat','ns','t1','s3://bucket/t1/meta.json');
INSERT INTO iceberg_tables VALUES ('cat','ns','t2','s3://bucket/t2/meta.json');
INSERT INTO iceberg_tables VALUES ('cat','other','t3','s3://bucket/t3/meta.json');
INSERT INTO iceberg_namespace_properties VALUES ('cat','ns','owner','data-eng');
INSERT INTO iceberg_namespace_properties VALUES ('cat','ns','location','s3://bucket/ns');
