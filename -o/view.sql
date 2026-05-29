-- ifcfast substrate convenience view.
-- Open both tables and join them so downstream queries that don't care
-- about instancing see one row per product with geometry attached.
--
--   duckdb -c ".read view.sql; SELECT class, COUNT(*) FROM products GROUP BY class;"
--
-- For hierarchical queries (the whole point of the split), read the
-- representations + instances tables directly:
--
--   SELECT i.class,
--          COUNT(*)                AS instance_count,
--          COUNT(DISTINCT i.rep_id) AS unique_shapes,
--          SUM(r.triangle_count)    AS total_triangle_bytes_if_expanded
--     FROM instances i
--     LEFT JOIN representations r USING (rep_id)
--    GROUP BY i.class
--    ORDER BY instance_count DESC;

CREATE OR REPLACE VIEW products AS
  SELECT
      i.*,
      r.source_kind        AS rep_source_kind,
      r.mesh_source        AS rep_mesh_source,
      r.vertex_count       AS rep_vertex_count,
      r.triangle_count     AS rep_triangle_count,
      r.vertices_le        AS rep_vertices_le,
      r.indices_le         AS rep_indices_le,
      r.segments           AS rep_segments,
      r.local_bbox_min_xyz AS rep_local_bbox_min_xyz,
      r.local_bbox_max_xyz AS rep_local_bbox_max_xyz
    FROM instances i
    LEFT JOIN representations r USING (rep_id);
