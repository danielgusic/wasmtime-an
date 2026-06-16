// dummy program to test if certain language features work with AN-encoding
// structs, traits, generics, trait objects, enums
#[derive(Clone, Copy)]
pub struct Point {
    x: i32,
    y: i32,
}

impl Point {
    fn new(x: i32, y: i32) -> Point {
        Point { x, y }
    }

    fn manhattan(&self) -> i32 {
        self.x.abs() + self.y.abs()
    }

    fn translate(&mut self, dx: i32, dy: i32) {
        self.x += dx;
        self.y += dy;
    }
}

#[derive(Clone, Copy)]
pub struct Rect {
    origin: Point,
    size: Point,
}

impl Rect {
    fn area(&self) -> i32 {
        self.size.x * self.size.y
    }

    fn far_corner(&self) -> Point {
        Point::new(self.origin.x + self.size.x, self.origin.y + self.size.y)
    }
}

#[unsafe(no_mangle)]
pub fn test_basic() -> i32 {
    let p = Point::new(3, -4);
    p.x + p.y + p.manhattan() // 3 + -4 + 7 = 6
}

#[unsafe(no_mangle)]
pub fn test_mutation() -> i32 {
    let mut p = Point::new(1, 2);
    p.x = 10; // direct field write
    p.translate(5, -3); // mutate via &mut self
    p.x + p.y // 15 + -1 = 14
}

#[unsafe(no_mangle)]
pub fn test_nested() -> i32 {
    let r = Rect {
        origin: Point::new(2, 3),
        size: Point::new(4, 5),
    };
    let corner = r.far_corner();
    r.area() + corner.x + corner.y // 20 + 6 + 8 = 34
}

fn by_value(mut p: Point) -> i32 {
    p.x += 100; // local copy, caller unaffected
    p.x
}

fn by_ref(p: &Point) -> i32 {
    p.x + p.y
}

#[unsafe(no_mangle)]
pub fn test_pass() -> i32 {
    let p = Point::new(7, 8);
    let v = by_value(p);
    let r = by_ref(&p);
    v + r + p.x // 107 + 15 + 7 = 129
}

#[unsafe(no_mangle)]
pub fn test_array() -> i32 {
    let mut pts = [Point::new(0, 0); 4];
    let mut i = 0;
    while i < 4 {
        pts[i] = Point::new(i as i32, (i as i32) * 2);
        i += 1;
    }
    pts[2].translate(10, 10); // mutate one element in place

    let mut sum = 0;
    for p in pts.iter() {
        sum += p.x + p.y;
    }
    sum // (0)+(1+2)+(2+10 + 4+10)+(3+6) = 0+3+26+9 = 38
}

#[derive(Clone, Copy)]
pub struct Vec3 {
    data: [i32; 3],
}

impl Vec3 {
    fn dot(&self, other: &Vec3) -> i32 {
        let mut acc = 0;
        let mut i = 0;
        while i < 3 {
            acc += self.data[i] * other.data[i];
            i += 1;
        }
        acc
    }

    fn scaled(&self, k: i32) -> Vec3 {
        Vec3 {
            data: [self.data[0] * k, self.data[1] * k, self.data[2] * k],
        }
    }
}

#[unsafe(no_mangle)]
pub fn test_vec3() -> i32 {
    let a = Vec3 { data: [1, 2, 3] };
    let b = Vec3 { data: [4, 5, 6] };
    let d = a.dot(&b); // 4 + 10 + 18 = 32
    let s = a.scaled(2); // [2,4,6]
    d + s.dot(&b) // 32 + (8+20+36=64) = 96
}

trait Shape {
    fn area(&self) -> i32;
    fn double_area(&self) -> i32 {
        self.area() * 2
    }
}

pub struct Square {
    side: i32,
}

pub struct Triangle {
    base: i32,
    height: i32,
}

impl Shape for Square {
    fn area(&self) -> i32 {
        self.side * self.side
    }
}

impl Shape for Triangle {
    fn area(&self) -> i32 {
        self.base * self.height / 2
    }
}

fn describe<S: Shape>(s: &S) -> i32 {
    s.area() + s.double_area()
}

#[unsafe(no_mangle)]
pub fn test_trait_static() -> i32 {
    let sq = Square { side: 4 };
    let tr = Triangle { base: 6, height: 5 };
    describe(&sq) + describe(&tr) // (16+32) + (15+30) = 48 + 45 = 93
}

#[unsafe(no_mangle)]
pub fn test_trait_dynamic() -> i32 {
    let shapes: [&dyn Shape; 2] = [&Square { side: 3 }, &Triangle { base: 8, height: 4 }];
    let mut sum = 0;
    for s in shapes.iter() {
        sum += s.double_area(); // vtable dispatch
    }
    let boxed: Box<dyn Shape> = Box::new(Square { side: 5 });
    sum + boxed.area() // (9*2=18)+(16*2=32) + 25 = 50 + 25 = 75
}

pub struct Pair<T> {
    first: T,
    second: T,
}

impl<T: Copy + core::ops::Add<Output = T>> Pair<T> {
    fn new(a: T, b: T) -> Pair<T> {
        Pair {
            first: a,
            second: b,
        }
    }
    fn combined(&self) -> T {
        self.first + self.second
    }
    fn swapped(self) -> Pair<T> {
        Pair {
            first: self.second,
            second: self.first,
        }
    }
}

#[unsafe(no_mangle)]
pub fn test_generic_struct() -> i32 {
    let p = Pair::new(11, 22);
    let combined = p.combined(); // 33
    let s = p.swapped(); // consumes p
    combined + s.first // 33 + 22 = 55
}

fn max_of<T: PartialOrd + Copy>(items: &[T]) -> T {
    let mut m = items[0];
    for &x in items.iter() {
        if x > m {
            m = x;
        }
    }
    m
}

#[unsafe(no_mangle)]
pub fn test_generic_fn() -> i32 {
    let nums = [3, 9, 2, 14, 7];
    let m = max_of(&nums); // 14
    let total: i32 = nums.iter().filter(|&&x| x > 5).sum(); // 9+14+7 = 30
    m + total // 44
}

pub enum Op {
    Add(i32, i32),
    Negate(i32),
    Const { value: i32 },
}

fn eval(op: &Op) -> i32 {
    match op {
        Op::Add(a, b) => a + b,
        Op::Negate(a) => -a,
        Op::Const { value } => *value,
    }
}

#[unsafe(no_mangle)]
pub fn test_enum() -> i32 {
    let ops = [Op::Add(4, 5), Op::Negate(3), Op::Const { value: 100 }];
    let mut sum = 0;
    for op in ops.iter() {
        sum += eval(op);
    }
    sum // 9 + -3 + 100 = 106
}

fn checked_div(a: i32, b: i32) -> Option<i32> {
    if b == 0 { None } else { Some(a / b) }
}

#[unsafe(no_mangle)]
pub fn test_option() -> i32 {
    let ok = checked_div(20, 4).unwrap_or(-1); // 5
    let bad = checked_div(1, 0).unwrap_or(-1); // -1
    let chained = checked_div(100, 5).map(|v| v + 1).unwrap_or(0); // 21
    ok + bad + chained // 5 + -1 + 21 = 25
}

#[unsafe(no_mangle)]
pub fn run_all() -> i32 {
    test_basic()
        + test_mutation()
        + test_nested()
        + test_pass()
        + test_array()
        + test_vec3()
        + test_trait_static()
        + test_trait_dynamic()
        + test_generic_struct()
        + test_generic_fn()
        + test_enum()
        + test_option()
    // 6+14+34+129+38+96 + 93+75+55+44+106+25 = 715
}

pub fn main() {
    println!("test_basic    = {} (expect 6)", test_basic());
    println!("test_mutation = {} (expect 14)", test_mutation());
    println!("test_nested   = {} (expect 34)", test_nested());
    println!("test_pass     = {} (expect 129)", test_pass());
    println!("test_array    = {} (expect 38)", test_array());
    println!("test_vec3     = {} (expect 96)", test_vec3());
    println!("test_trait_static  = {} (expect 93)", test_trait_static());
    println!("test_trait_dynamic = {} (expect 75)", test_trait_dynamic());
    println!("test_generic_struct= {} (expect 55)", test_generic_struct());
    println!("test_generic_fn    = {} (expect 44)", test_generic_fn());
    println!("test_enum          = {} (expect 106)", test_enum());
    println!("test_option        = {} (expect 25)", test_option());
    println!("run_all       = {} (expect 715)", run_all());
}
