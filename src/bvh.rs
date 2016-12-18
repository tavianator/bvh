//! This module defines a [`BVH`] building procedure as well as a [`BVH`] flattening procedure
//! so that the recursive structure can be easily used in compute shaders.
//!
//! [`BVH`]: struct.BVH.html
//!

use std::boxed::Box;
use std::f32;
use std::iter::repeat;

use EPSILON;
use aabb::{AABB, Bounded};
use bounding_hierarchy::BoundingHierarchy;
use ray::Ray;

/// Enum which describes the union type of a node in a [`BVH`].
/// This structure does not allow for storing a root node's [`AABB`]. Therefore rays
/// which do not hit the root [`AABB`] perform two [`AABB`] tests (left/right) instead of one.
/// On the other hand this structure decreases the total number of indirections when traversing
/// the BVH. Only those nodes are accessed, which are definetely hit.
///
/// [`AABB`]: ../aabb/struct.AABB.html
/// [`BVH`]: struct.BVH.html
///
pub enum BVHNode {
    /// Leaf node.
    Leaf {
        /// The shapes contained in this leaf.
        shapes: Vec<usize>,
    },
    /// Inner node.
    Node {
        /// The convex hull of the shapes' `AABB`s in child_l.
        child_l_aabb: AABB,

        /// Left subtree.
        child_l: Box<BVHNode>,

        /// The convex hull of the shapes' `AABB`s in child_r.
        child_r_aabb: AABB,

        /// Right subtree.
        child_r: Box<BVHNode>,
    },
}

impl BVHNode {
    /// Builds a [`BVHNode`] recursively using SAH partitioning.
    ///
    /// [`BVHNode`]: enum.BVHNode.html
    ///
    pub fn build<T: Bounded>(shapes: &[T], indices: Vec<usize>) -> BVHNode {
        // Helper function to accumulate the AABB joint and the centroids AABB
        fn grow_convex_hull(convex_hull: (AABB, AABB), shape_aabb: &AABB) -> (AABB, AABB) {
            let center = &shape_aabb.center();
            let convex_hull_aabbs = &convex_hull.0;
            let convex_hull_centroids = &convex_hull.1;
            (convex_hull_aabbs.join(shape_aabb), convex_hull_centroids.grow(center))
        }

        let mut convex_hull = Default::default();
        for index in &indices {
            convex_hull = grow_convex_hull(convex_hull, &shapes[*index].aabb());
        }
        let (aabb_bounds, centroid_bounds) = convex_hull;

        // If there are five or fewer elements, don't split anymore
        if indices.len() <= 5 {
            return BVHNode::Leaf { shapes: indices };
        }

        // Find the axis along which the shapes are spread the most
        let split_axis = centroid_bounds.largest_axis();
        let split_axis_size = centroid_bounds.max[split_axis] - centroid_bounds.min[split_axis];

        if split_axis_size < EPSILON {
            return BVHNode::Leaf { shapes: indices };
        }

        /// Defines a Bucket utility object.
        #[derive(Copy, Clone)]
        struct Bucket {
            size: usize,
            aabb: AABB,
        }

        impl Bucket {
            /// Returns an empty bucket.
            fn empty() -> Bucket {
                Bucket {
                    size: 0,
                    aabb: AABB::empty(),
                }
            }

            /// Extends this `Bucket` by the given `AABB`.
            fn add_aabb(&mut self, aabb: &AABB) {
                self.size += 1;
                self.aabb = self.aabb.join(aabb);
            }
        }

        /// Joins two `Bucket`s.
        fn join_bucket(a: Bucket, b: &Bucket) -> Bucket {
            Bucket {
                size: a.size + b.size,
                aabb: a.aabb.join(&b.aabb),
            }
        }

        // Create six buckets, and six index assignment vectors
        const NUM_BUCKETS: usize = 6;
        let mut buckets = [Bucket::empty(); NUM_BUCKETS];
        let mut bucket_assignments: [Vec<usize>; NUM_BUCKETS] = Default::default();

        // Assign each shape to a bucket
        for idx in &indices {
            let shape = &shapes[*idx];
            let shape_aabb = shape.aabb();
            let shape_center = shape_aabb.center();

            // Get the relative position of the shape centroid [0.0..1.0]
            let bucket_num_relative = (shape_center[split_axis] - centroid_bounds.min[split_axis]) /
                                      split_axis_size;

            // Convert that to the actual `Bucket` number
            let bucket_num = (bucket_num_relative * (NUM_BUCKETS as f32 - 0.01)) as usize;

            // Extend the selected `Bucket` and add the index to the actual bucket
            buckets[bucket_num].add_aabb(&shape_aabb);
            bucket_assignments[bucket_num].push(*idx);
        }

        // Compute the costs for each configuration and
        // select the configuration with the minimal costs
        let mut min_bucket = 0;
        let mut min_cost = f32::INFINITY;
        let mut child_l_aabb = AABB::empty();
        let mut child_r_aabb = AABB::empty();
        for i in 0..(NUM_BUCKETS - 1) {
            let child_l = buckets.iter().take(i + 1).fold(Bucket::empty(), join_bucket);
            let child_r = buckets.iter().skip(i + 1).fold(Bucket::empty(), join_bucket);

            let cost = (child_l.size as f32 * child_l.aabb.surface_area() +
                        child_r.size as f32 * child_r.aabb.surface_area()) /
                       aabb_bounds.surface_area();

            if cost < min_cost {
                min_bucket = i;
                min_cost = cost;
                child_l_aabb = child_l.aabb;
                child_r_aabb = child_r.aabb;
            }
        }

        // Join together all index buckets, and proceed recursively
        let mut child_l_indices = Vec::new();
        for mut indices in bucket_assignments.iter_mut().take(min_bucket + 1) {
            child_l_indices.append(&mut indices);
        }
        let mut child_r_indices = Vec::new();
        for mut indices in bucket_assignments.iter_mut().skip(min_bucket + 1) {
            child_r_indices.append(&mut indices);
        }

        // Construct the actual data structure
        BVHNode::Node {
            child_l_aabb: child_l_aabb,
            child_l: Box::new(BVHNode::build(shapes, child_l_indices)),
            child_r_aabb: child_r_aabb,
            child_r: Box::new(BVHNode::build(shapes, child_r_indices)),
        }
    }

    /// Prints a textual representation of the recursive [`BVH`] structure.
    ///
    /// [`BVH`]: struct.BVH.html
    ///
    fn pretty_print(&self, depth: usize) {
        let padding: String = repeat(" ").take(depth).collect();
        match *self {
            BVHNode::Node { ref child_l, ref child_r, .. } => {
                println!("{}child_l", padding);
                child_l.pretty_print(depth + 1);
                println!("{}child_r", padding);
                child_r.pretty_print(depth + 1);
            }
            BVHNode::Leaf { ref shapes } => {
                println!("{}shapes\t{:?}", padding, shapes);
            }
        }
    }

    /// Traverses the [`BVH`] recursively and insterts shapes which are hit with a
    /// high probability by `ray` into the [`Vec`] `indices`.
    ///
    /// [`BVH`]: struct.BVH.html
    /// [`Vec`]: https://doc.rust-lang.org/std/vec/struct.Vec.html
    ///
    pub fn traverse_recursive(&self, ray: &Ray, indices: &mut Vec<usize>) {
        match *self {
            BVHNode::Node { ref child_l_aabb, ref child_l, ref child_r_aabb, ref child_r } => {
                if ray.intersects_aabb(child_l_aabb) {
                    child_l.traverse_recursive(ray, indices);
                }
                if ray.intersects_aabb(child_r_aabb) {
                    child_r.traverse_recursive(ray, indices);
                }
            }
            BVHNode::Leaf { ref shapes } => {
                for index in shapes {
                    indices.push(*index);
                }
            }
        }
    }
}

/// The [`BVH`] data structure. Only contains the root node of the [`BVH`] tree.
///
/// [`BVH`]: struct.BVH.html
///
pub struct BVH {
    /// The root node of the [`BVH`].
    ///
    /// [`BVH`]: struct.BVH.html
    ///
    pub root: BVHNode,
}

impl BVH {
    /// Creates a new [`BVH`] from the `shapes` slice.
    ///
    /// [`BVH`]: struct.BVH.html
    ///
    /// # Example
    ///
    /// ```
    /// use bvh::aabb::{AABB, Bounded};
    /// use bvh::bvh::BVH;
    /// use bvh::nalgebra::{Point3, Vector3};
    /// # fn create_bounded_shapes() -> Vec<AABB> {
    /// #     let mut shapes = Vec::new();
    /// #     let offset = Vector3::new(1.0, 1.0, 1.0);
    /// #     for i in 0..1000u32 {
    /// #         let position = Point3::new(i as f32, i as f32, i as f32);
    /// #         shapes.push(AABB::with_bounds(position - offset, position + offset));
    /// #     }
    /// #     shapes
    /// # }
    ///
    /// let shapes = create_bounded_shapes();
    /// let bvh = BVH::build(&shapes);
    /// ```
    pub fn build<T: Bounded>(shapes: &[T]) -> BVH {
        let indices = (0..shapes.len()).collect::<Vec<usize>>();
        let root = BVHNode::build(shapes, indices);
        BVH { root: root }
    }
}

impl BoundingHierarchy for BVH {
    fn build<T: Bounded>(shapes: &[T]) -> BVH {
        let indices = (0..shapes.len()).collect::<Vec<usize>>();
        let root = BVHNode::build(shapes, indices);
        BVH { root: root }
    }

    /// Traverses the tree recursively. Returns a subset of `shapes`, in which the [`AABB`]s
    /// of the elements were hit by the `ray`.
    ///
    /// [`AABB`]: ../aabb/struct.AABB.html
    ///
    /// # Example
    ///
    /// ```
    /// use bvh::aabb::{AABB, Bounded};
    /// use bvh::bounding_hierarchy::BoundingHierarchy;
    /// use bvh::bvh::BVH;
    /// use bvh::nalgebra::{Point3, Vector3};
    /// use bvh::ray::Ray;
    /// # fn create_bounded_shapes() -> Vec<AABB> {
    /// #     let mut shapes = Vec::new();
    /// #     let offset = Vector3::new(1.0, 1.0, 1.0);
    /// #     for i in 0..1000u32 {
    /// #         let position = Point3::new(i as f32, i as f32, i as f32);
    /// #         shapes.push(AABB::with_bounds(position - offset, position + offset));
    /// #     }
    /// #     shapes
    /// # }
    ///
    /// let origin = Point3::new(0.0,0.0,0.0);
    /// let direction = Vector3::new(1.0,0.0,0.0);
    /// let ray = Ray::new(origin, direction);
    /// let shapes = create_bounded_shapes();
    /// let bvh = BVH::build(&shapes);
    /// let hit_sphere_aabbs = bvh.traverse(&ray, &shapes);
    /// ```
    fn traverse<'a, T: Bounded>(&'a self, ray: &Ray, shapes: &'a [T]) -> Vec<&T> {
        let mut indices = Vec::new();
        self.root.traverse_recursive(ray, &mut indices);
        let mut hit_shapes = Vec::new();
        for index in &indices {
            let shape = &shapes[*index];
            if ray.intersects_aabb(&shape.aabb()) {
                hit_shapes.push(shape);
            }
        }
        hit_shapes
    }

    /// Prints the [`BVH`] in a tree-like visualization.
    ///
    /// [`BVH`]: struct.BVH.html
    ///
    fn pretty_print(&self) {
        self.root.pretty_print(0);
    }
}

#[cfg(test)]
pub mod tests {
    use bvh::BVH;
    use testbase::{build_some_bh, traverse_some_bh, build_1200_triangles_bh,
                   build_12k_triangles_bh, build_120k_triangles_bh, intersect_1200_triangles_bh,
                   intersect_12k_triangles_bh, intersect_120k_triangles_bh};

    #[test]
    /// Tests whether the building procedure succeeds in not failing.
    fn test_build_bvh() {
        build_some_bh::<BVH>();
    }

    #[test]
    /// Runs some primitive tests for intersections of a ray with a fixed scene given as a BVH.
    fn test_traverse_bvh() {
        traverse_some_bh::<BVH>();
    }

    #[bench]
    /// Benchmark the construction of a `BVH` with 1,200 triangles.
    fn bench_build_1200_triangles_bvh(mut b: &mut ::test::Bencher) {
        build_1200_triangles_bh::<BVH>(&mut b);
    }

    #[bench]
    /// Benchmark the construction of a `BVH` with 12,000 triangles.
    fn bench_build_12k_triangles_bvh(mut b: &mut ::test::Bencher) {
        build_12k_triangles_bh::<BVH>(&mut b);
    }

    #[bench]
    /// Benchmark the construction of a `BVH` with 120,000 triangles.
    fn bench_build_120k_triangles_bvh(mut b: &mut ::test::Bencher) {
        build_120k_triangles_bh::<BVH>(&mut b);
    }

    #[bench]
    /// Benchmark intersecting 1,200 triangles using the recursive `BVH`.
    fn bench_intersect_1200_triangles_bvh(mut b: &mut ::test::Bencher) {
        intersect_1200_triangles_bh::<BVH>(&mut b);
    }

    #[bench]
    /// Benchmark intersecting 12,000 triangles using the recursive `BVH`.
    fn bench_intersect_12k_triangles_bvh(mut b: &mut ::test::Bencher) {
        intersect_12k_triangles_bh::<BVH>(&mut b);
    }

    #[bench]
    /// Benchmark intersecting 120,000 triangles using the recursive `BVH`.
    fn bench_intersect_120k_triangles_bvh(mut b: &mut ::test::Bencher) {
        intersect_120k_triangles_bh::<BVH>(&mut b);
    }
}
