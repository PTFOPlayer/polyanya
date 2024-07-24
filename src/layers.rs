#[cfg(feature = "tracing")]
use tracing::instrument;

use bvh2d::bvh2d::BVH2d;
use glam::{vec2, Vec2};

use crate::{helpers::Vec2Helper, instance::EdgeSide, BoundedPolygon, MeshError, Polygon, Vertex};

/// Layer of a NavMesh
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Layer {
    /// List of `Vertex` in this mesh
    pub vertices: Vec<Vertex>,
    /// List of `Polygons` in this mesh
    pub polygons: Vec<Polygon>,
    pub(crate) baked_polygons: Option<BVH2d>,
    pub(crate) islands: Option<Vec<usize>>,
}

impl Layer {
    /// Remove pre-computed optimizations from the mesh. Call this if you modified the [`Mesh`].
    #[inline]
    pub fn unbake(&mut self) {
        self.baked_polygons = None;
        self.islands = None;
    }

    /// Pre-compute optimizations on the mesh
    ///
    /// Must be called on an unstitched layer.
    ///
    /// Optimisations available are:
    /// - [`Self::bake_polygon_finder`]
    /// - [`Self::bake_islands_detection`]
    pub fn bake(&mut self) {
        self.bake_polygon_finder();
        self.bake_islands_detection()
    }

    /// Speed up bailing out if two points are not reachable.
    ///
    /// This is useful if there are isolated zones in the mesh, and you need to check for a path
    /// between them.
    #[cfg_attr(feature = "tracing", instrument(skip_all))]
    pub fn bake_islands_detection(&mut self) {
        let mut islands = vec![usize::MAX; self.polygons.len()];
        while let Some((root, _)) = islands
            .iter()
            .enumerate()
            .find(|(_, island)| **island == usize::MAX)
        {
            let mut to_visit = Vec::new();
            to_visit.push(root);
            while let Some(next) = to_visit.pop() {
                if islands[next] == usize::MAX {
                    let polygon = &mut self.polygons[next];
                    islands[next] = root;
                    to_visit.extend(
                        polygon
                            .vertices
                            .iter()
                            .flat_map(|v| self.vertices[*v as usize].polygons.iter())
                            .filter_map(|i| {
                                if *i != u32::MAX {
                                    Some(*i as usize)
                                } else {
                                    None
                                }
                            }),
                    );
                }
            }
        }
        self.islands = Some(islands);
    }

    /// Speed up finding which polygon, if any, contains a point in the mesh.
    ///
    /// Uses a BVH. This is useful at the start of the pathfinding, to get the containing polygons
    /// for the start and end point. It can also be used through [`Self::point_in_mesh`] to check
    /// if a point is in the mesh.
    #[cfg_attr(feature = "tracing", instrument(skip_all))]
    pub fn bake_polygon_finder(&mut self) {
        let bounded_polygons = self
            .polygons
            .iter_mut()
            .map(|polygon| BoundedPolygon {
                aabb: polygon.vertices.iter().fold(
                    (vec2(f32::MAX, f32::MAX), Vec2::ZERO),
                    |mut aabb, v| {
                        if let Some(v) = self.vertices.get(*v as usize) {
                            if v.coords.x < aabb.0.x {
                                aabb.0.x = v.coords.x;
                            }
                            if v.coords.y < aabb.0.y {
                                aabb.0.y = v.coords.y;
                            }
                            if v.coords.x > aabb.1.x {
                                aabb.1.x = v.coords.x;
                            }
                            if v.coords.y > aabb.1.y {
                                aabb.1.y = v.coords.y;
                            }
                        }
                        aabb
                    },
                ),
            })
            .collect::<Vec<_>>();

        self.baked_polygons = Some(BVH2d::build(&bounded_polygons));
    }

    /// Create a `Layer` from a list of [`Vertex`] and [`Polygon`].
    pub fn new(vertices: Vec<Vertex>, polygons: Vec<Polygon>) -> Result<Self, MeshError> {
        if vertices.is_empty() || polygons.is_empty() {
            return Err(MeshError::EmptyMesh);
        }
        if polygons.len() > (2_i32.pow(24) - 1) as usize {
            return Err(MeshError::TooManyPolygons);
        }
        let mut layer = Layer {
            vertices,
            polygons,
            ..Default::default()
        };
        #[cfg(not(feature = "no-default-baking"))]
        layer.bake();
        // just to not get a warning on the mut borrow. should be pretty much free anyway
        #[cfg(feature = "no-default-baking")]
        layer.unbake();
        Ok(layer)
    }

    #[cfg_attr(feature = "tracing", instrument(skip_all))]
    pub(crate) fn get_point_location_unit(&self, point: Vec2) -> u32 {
        for (i, polygon) in self.polygons.iter().enumerate() {
            if self.point_in_polygon(point, polygon) {
                return i as u32;
            }
        }
        u32::MAX
    }

    #[cfg_attr(feature = "tracing", instrument(skip_all))]
    pub(crate) fn get_point_location_unit_baked(&self, point: Vec2) -> u32 {
        self.baked_polygons
            .as_ref()
            .unwrap()
            .contains_iterator(&point)
            .find(|index| self.point_in_polygon(point, &self.polygons[*index]))
            .map(|index| index as u32)
            .unwrap_or(u32::MAX)
    }

    #[cfg_attr(feature = "tracing", instrument(skip_all))]
    #[inline(always)]
    fn point_in_polygon(&self, point: Vec2, polygon: &Polygon) -> bool {
        let mut edged = false;
        for edge in polygon.edges_index().iter() {
            if edge.0.max(edge.1) as usize >= self.vertices.len() {
                return false;
            }
            edged = true;
            // Bounds are checked just before
            #[allow(unsafe_code)]
            let (last, next) = unsafe {
                (
                    self.vertices.get_unchecked(edge.0 as usize).coords,
                    self.vertices.get_unchecked(edge.1 as usize).coords,
                )
            };

            let current_side = point.side((last, next));
            if current_side == EdgeSide::Edge && point.on_segment((last, next)) {
                return true;
            }
            if current_side != EdgeSide::Left {
                return false;
            }
        }
        if edged {
            return true;
        }
        false
    }

    #[cfg_attr(feature = "tracing", instrument(skip_all))]
    pub(crate) fn get_point_location(&self, point: Vec2, delta: f32) -> Option<u32> {
        [
            vec2(0.0, 0.0),
            vec2(delta, 0.0),
            vec2(delta, delta),
            vec2(0.0, delta),
            vec2(-delta, delta),
            vec2(-delta, 0.0),
            vec2(-delta, -delta),
            vec2(0.0, -delta),
            vec2(delta, -delta),
        ]
        .iter()
        .map(|delta| {
            if self.baked_polygons.is_none() {
                self.get_point_location_unit(point + *delta)
            } else {
                self.get_point_location_unit_baked(point + *delta)
            }
        })
        .find(|poly| *poly != u32::MAX)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    #[cfg(feature = "detailed-layers")]
    use crate::helpers::line_intersect_segment;
    use crate::{instance::U32Layer, Coords, Layer, Mesh, Path, Polygon, SearchNode, Vertex};
    use glam::{vec2, IVec2, Vec2};

    fn mesh_u_grid() -> Mesh {
        let main_layer = Layer {
            vertices: vec![
                Vertex::new(vec2(0., 0.), vec![0, u32::MAX]),
                Vertex::new(vec2(1., 0.), vec![0, 1, u32::MAX]),
                Vertex::new(vec2(2., 0.), vec![1, 2, u32::MAX]),
                Vertex::new(vec2(3., 0.), vec![2, u32::MAX]),
                Vertex::new(vec2(0., 1.), vec![0, u32::MAX]),
                Vertex::new(vec2(1., 1.), vec![1, 0, u32::MAX]),
                Vertex::new(vec2(2., 1.), vec![2, 1, u32::MAX]),
                Vertex::new(vec2(3., 1.), vec![2, u32::MAX]),
            ],
            polygons: vec![
                Polygon::new(vec![0, 1, 5, 4], false),
                Polygon::new(vec![1, 2, 6, 5], false),
                Polygon::new(vec![2, 3, 7, 6], false),
            ],
            ..Default::default()
        };
        let mut mesh = Mesh {
            layers: vec![
                main_layer,
                Layer {
                    vertices: vec![
                        Vertex::new(vec2(0., 1.), vec![0, u32::MAX]),
                        Vertex::new(vec2(1., 1.), vec![0, u32::MAX]),
                        Vertex::new(vec2(0., 2.), vec![0, u32::MAX]),
                        Vertex::new(vec2(1., 2.), vec![0, u32::MAX]),
                    ],
                    polygons: vec![Polygon::new(vec![0, 1, 3, 2], true)],
                    ..Default::default()
                },
                Layer {
                    vertices: vec![
                        Vertex::new(vec2(2., 1.), vec![0, u32::MAX]),
                        Vertex::new(vec2(3., 1.), vec![0, u32::MAX]),
                        Vertex::new(vec2(2., 2.), vec![0, u32::MAX]),
                        Vertex::new(vec2(3., 2.), vec![0, u32::MAX]),
                    ],
                    polygons: vec![Polygon::new(vec![0, 1, 3, 2], true)],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        mesh.bake();
        mesh.stitch_at_points(vec![
            ((0, 1), vec![vec2(0., 1.), vec2(1., 1.)]),
            ((0, 2), vec![vec2(2., 1.), vec2(3., 1.)]),
        ]);
        mesh
    }

    #[test]
    fn point_in_polygon() {
        let mesh = mesh_u_grid();
        assert_eq!(mesh.get_point_location(vec2(0.5, 0.5)), 0);
        assert_eq!(mesh.get_point_location(vec2(1.5, 0.5)), 1);
        assert_eq!(
            mesh.get_point_location(vec2(0.5, 1.5)),
            u32::from_layer_and_polygon(1, 0)
        );
        assert_eq!(mesh.get_point_location(vec2(1.5, 1.5)), u32::MAX);
        assert_eq!(
            mesh.get_point_location(vec2(2.5, 1.5)),
            u32::from_layer_and_polygon(2, 0)
        );
    }

    #[test]
    fn successors_straight_line() {
        let mesh = mesh_u_grid();

        let from: Vec2 = vec2(0.1, 1.1);
        let to = vec2(1.1, 0.1);
        let search_node = SearchNode {
            path: vec![],
            #[cfg(feature = "detailed-layers")]
            path_with_layers: vec![],
            root: from,
            interval: (vec2(0.0, 1.0), vec2(1.0, 1.0)),
            edge: (0, 1),
            polygon_from: mesh.get_point_location(from),
            polygon_to: mesh.get_point_location(to),
            previous_polygon_layer: 0,
            f: 0.0,
            g: from.distance(to),
        };
        let successors = dbg!(mesh.successors(search_node, to));
        assert_eq!(successors.len(), 0);
        assert_eq!(
            mesh.path(from, to).unwrap(),
            Path {
                path: vec![to],
                length: from.distance(to),
                #[cfg(feature = "detailed-layers")]
                path_with_layers: vec![(to, 0)],
            }
        );
    }

    #[test]
    fn successors_corner_first_step() {
        let mesh = mesh_u_grid();

        let from = vec2(0.1, 1.9);
        let to = vec2(2.1, 1.9);
        let search_node = SearchNode {
            path: vec![],
            #[cfg(feature = "detailed-layers")]
            path_with_layers: vec![],
            root: from,
            interval: (vec2(0.0, 1.0), vec2(1.0, 1.0)),
            edge: (4, 5),
            polygon_from: mesh.get_point_location(from),
            polygon_to: 0,
            previous_polygon_layer: 0,
            f: 0.0,
            g: from.distance(to),
        };
        let successors = dbg!(mesh.successors(search_node, to));
        assert_eq!(successors.len(), 1);
        assert_eq!(successors[0].root, vec2(2.0, 1.0));
        assert_eq!(
            successors[0].f,
            from.distance(vec2(1.0, 1.0)) + vec2(1.0, 1.0).distance(vec2(2.0, 1.0))
        );
        assert_eq!(successors[0].g, vec2(2.0, 1.0).distance(to));
        assert_eq!(successors[0].polygon_from.polygon(), 2);
        assert_eq!(successors[0].polygon_to, u32::from_layer_and_polygon(2, 0));
        assert_eq!(successors[0].interval, (vec2(3.0, 1.0), vec2(2.0, 1.0)));
        assert_eq!(successors[0].edge, (7, 6));
        assert_eq!(successors[0].path, vec![vec2(1.0, 1.0), vec2(2.0, 1.0)]);

        assert_eq!(
            mesh.path(from, to).unwrap(),
            Path {
                path: vec![vec2(1.0, 1.0), vec2(2.0, 1.0), to],
                length: from.distance(vec2(1.0, 1.0))
                    + vec2(1.0, 1.0).distance(vec2(2.0, 1.0))
                    + vec2(2.0, 1.0).distance(to),
                #[cfg(feature = "detailed-layers")]
                path_with_layers: vec![(vec2(1.0, 1.0), 0), (vec2(2.0, 1.0), 2), (to, 2)],
            }
        );
    }

    /// layer 1:
    /// 000
    ///   1
    ///   222
    ///
    /// layer 2:
    ///
    /// 00000
    ///
    fn mesh_overlapping_layers() -> Mesh {
        let main_layer = Layer {
            vertices: vec![
                Vertex::new(vec2(0., 3.), vec![0, u32::MAX]),
                Vertex::new(vec2(3., 3.), vec![0, u32::MAX]),
                Vertex::new(vec2(0., 2.), vec![0, u32::MAX]),
                Vertex::new(vec2(1., 2.), vec![0, u32::MAX]),
                Vertex::new(vec2(2., 2.), vec![0, 1, u32::MAX]),
                Vertex::new(vec2(3., 2.), vec![0, 1, u32::MAX]),
                Vertex::new(vec2(2., 1.), vec![1, 2, u32::MAX]),
                Vertex::new(vec2(3., 1.), vec![1, 2, u32::MAX]),
                Vertex::new(vec2(4., 1.), vec![2, u32::MAX]),
                Vertex::new(vec2(5., 1.), vec![2, u32::MAX]),
                Vertex::new(vec2(2., 0.), vec![2, u32::MAX]),
                Vertex::new(vec2(5., 0.), vec![2, u32::MAX]),
            ],
            polygons: vec![
                Polygon::new(vec![2, 3, 4, 5, 1, 0], false),
                Polygon::new(vec![6, 7, 5, 4], false),
                Polygon::new(vec![10, 11, 9, 8, 7, 6], false),
            ],
            ..Default::default()
        };
        let overlapping_layer = Layer {
            vertices: vec![
                Vertex::new(vec2(0., 2.), vec![0, u32::MAX]),
                Vertex::new(vec2(1., 2.), vec![0, u32::MAX]),
                Vertex::new(vec2(5., 2.), vec![0, u32::MAX]),
                Vertex::new(vec2(0., 1.), vec![0, u32::MAX]),
                Vertex::new(vec2(4., 1.), vec![0, u32::MAX]),
                Vertex::new(vec2(5., 1.), vec![0, u32::MAX]),
            ],
            polygons: vec![Polygon::new(vec![3, 4, 5, 2, 1, 0], false)],
            ..Default::default()
        };
        let mut mesh = Mesh {
            layers: vec![main_layer, overlapping_layer],
            ..Default::default()
        };
        mesh.bake();
        let points = dbg!(mesh.find_stitch_points());
        mesh.stitch_at_points(points);
        mesh
    }

    fn reduce_path_precision(path: Vec<(Vec2, u8)>) -> Vec<(IVec2, u8)> {
        path.into_iter()
            .map(|(point, layer)| ((point * 100000.0).as_ivec2(), layer))
            .collect()
    }

    #[test]
    fn shortcut_blocked() {
        let mesh = mesh_overlapping_layers();
        for i in 0..15 {
            let from = vec2(i as f32 / 10.0, 2.1);
            let to = vec2(5.0 - i as f32 / 10.0, 0.9);
            let mut blocked = HashSet::default();
            blocked.insert(1);
            let path = dbg!(mesh.path_on_layers(from, to, blocked).unwrap());
            assert_eq!(path.path, vec![vec2(2.0, 2.0), vec2(3.0, 1.0), to]);
            #[cfg(feature = "detailed-layers")]
            assert_eq!(
                path.path_with_layers,
                vec![(vec2(2.0, 2.0), 0), (vec2(3.0, 1.0), 0), (to, 0)]
            );
        }
    }

    #[test]
    fn take_shortcut() {
        let mesh = mesh_overlapping_layers();
        for i in 0..6 {
            let from = vec2(i as f32 / 10.0, 2.1);
            let to = vec2(5.0 - i as f32 / 10.0, 0.9);
            let path = dbg!(mesh.path(from, to).unwrap());
            assert_eq!(path.path, vec![to]);
            #[cfg(feature = "detailed-layers")]
            assert_eq!(
                reduce_path_precision(path.path_with_layers),
                reduce_path_precision(vec![
                    (
                        line_intersect_segment((from, to), (vec2(0.0, 2.0), vec2(5.0, 2.0)))
                            .unwrap(),
                        1
                    ),
                    (
                        line_intersect_segment((from, to), (vec2(0.0, 1.0), vec2(5.0, 1.0)))
                            .unwrap(),
                        0
                    ),
                    (to, 0)
                ]),
            );
        }
    }

    #[test]
    fn take_shortcut_back() {
        let mesh = mesh_overlapping_layers();
        for i in 0..6 {
            let from = vec2(5.0 - i as f32 / 10.0, 0.9);
            let to = vec2(i as f32 / 10.0, 2.1);
            let path = dbg!(mesh.path(from, to).unwrap());
            assert_eq!(path.path, vec![to]);
            #[cfg(feature = "detailed-layers")]
            assert_eq!(
                reduce_path_precision(path.path_with_layers),
                reduce_path_precision(vec![
                    (
                        line_intersect_segment((from, to), (vec2(0.0, 1.0), vec2(5.0, 1.0)))
                            .unwrap(),
                        1
                    ),
                    (
                        line_intersect_segment((from, to), (vec2(0.0, 2.0), vec2(5.0, 2.0)))
                            .unwrap(),
                        0
                    ),
                    (to, 0)
                ]),
            );
        }
    }

    #[test]
    fn shortcut_with_corner() {
        let mesh = mesh_overlapping_layers();
        for i in 7..8 {
            let from = vec2(i as f32 / 10.0, 2.1);
            let to = vec2(5.0 - i as f32 / 10.0, 0.9);
            let path = dbg!(mesh.path(from, to).unwrap());
            match i {
                7 => {
                    assert_eq!(path.path, vec![vec2(1.0, 2.0), to]);
                    #[cfg(feature = "detailed-layers")]
                    assert_eq!(
                        path.path_with_layers,
                        vec![(vec2(1.0, 2.0), 1), (vec2(4.0, 1.0), 0), (to, 0)]
                    );
                }
                _ if i < 11 => {
                    assert_eq!(path.path, vec![vec2(1.0, 2.0), vec2(4.0, 1.0), to]);
                    #[cfg(feature = "detailed-layers")]
                    assert_eq!(
                        path.path_with_layers,
                        vec![(vec2(1.0, 2.0), 1), (vec2(4.0, 1.0), 0), (to, 0)]
                    );
                }
                _ if i < 15 => {
                    assert_eq!(path.path, vec![vec2(2.0, 2.0), vec2(3.0, 1.0), to]);
                    #[cfg(feature = "detailed-layers")]
                    assert_eq!(
                        path.path_with_layers,
                        vec![(vec2(2.0, 2.0), 0), (vec2(3.0, 1.0), 0), (to, 0)]
                    );
                }
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn shortcut_with_corner_back() {
        let mesh = mesh_overlapping_layers();
        for i in 7..15 {
            let from = vec2(5.0 - i as f32 / 10.0, 0.9);
            let to = vec2(i as f32 / 10.0, 2.1);
            let path = dbg!(mesh.path(from, to).unwrap());
            match i {
                7 => {
                    assert_eq!(path.path, vec![vec2(4.0, 1.0), to]);
                    #[cfg(feature = "detailed-layers")]
                    assert_eq!(
                        path.path_with_layers,
                        vec![(vec2(4.0, 1.0), 1), (vec2(0.9999997, 2.0), 0), (to, 0)]
                    );
                }
                _ if i < 11 => {
                    assert_eq!(path.path, vec![vec2(4.0, 1.0), vec2(1.0, 2.0), to]);
                    #[cfg(feature = "detailed-layers")]
                    assert_eq!(
                        path.path_with_layers,
                        vec![(vec2(4.0, 1.0), 1), (vec2(1.0, 2.0), 0), (to, 0)]
                    );
                }
                _ if i < 15 => {
                    assert_eq!(path.path, vec![vec2(3.0, 1.0), vec2(2.0, 2.0), to]);
                    #[cfg(feature = "detailed-layers")]
                    assert_eq!(
                        path.path_with_layers,
                        vec![(vec2(3.0, 1.0), 0), (vec2(2.0, 2.0), 0), (to, 0)]
                    );
                }
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn from_one_to_the_other() {
        let mesh = mesh_overlapping_layers();
        let path = dbg!(mesh
            .path(
                Coords {
                    pos: vec2(2.5, 1.5),
                    layer: Some(0)
                },
                Coords {
                    pos: vec2(2.5, 1.5),
                    layer: Some(1)
                },
            )
            .unwrap());
        assert_eq!(
            path.path,
            vec![vec2(3.0, 1.0,), vec2(4.0, 1.0,), vec2(2.5, 1.5,),],
        );
        #[cfg(feature = "detailed-layers")]
        assert_eq!(
            path.path_with_layers,
            vec![
                (vec2(3.0, 1.0), 0),
                (vec2(4.0, 1.0), 1),
                (vec2(2.5, 1.5), 1),
            ],
        );

        let path_back = dbg!(mesh
            .path(
                Coords {
                    pos: vec2(2.5, 1.5),
                    layer: Some(1)
                },
                Coords {
                    pos: vec2(2.5, 1.5),
                    layer: Some(0)
                },
            )
            .unwrap());
        assert_eq!(
            path_back.path,
            vec![vec2(4.0, 1.0,), vec2(3.0, 1.0,), vec2(2.5, 1.5,),],
        );
        #[cfg(feature = "detailed-layers")]
        assert_eq!(
            path_back.path_with_layers,
            vec![
                (vec2(4.0, 1.0), 0),
                (vec2(3.0, 1.0), 0),
                (vec2(2.5, 1.5), 0),
            ],
        );
    }

    #[test]
    fn find_point_on_layer() {
        let mesh = mesh_overlapping_layers();
        assert_eq!(
            mesh.get_point_location(Coords {
                pos: vec2(2.5, 1.5),
                layer: Some(0)
            }),
            1
        );
        assert_eq!(
            mesh.get_point_location(Coords {
                pos: vec2(2.5, 1.5),
                layer: Some(1)
            }),
            u32::from_layer_and_polygon(1, 0)
        );
    }
}
