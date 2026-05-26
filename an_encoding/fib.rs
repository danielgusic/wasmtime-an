#[unsafe(no_mangle)]
pub fn fib(num: i32) -> i32 {
    let mut a: i32 = 0;
    let mut b: i32 = 1;
    let mut tmp: i32;
    for _ in 0..num {
        tmp = a + b;
        a = b;
        b = tmp;
    }
    b
}

use std::io::{self, BufRead};

pub fn main() {
    println!("Enter a number");
    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        println!("{}", fib(line.unwrap().parse::<i32>().unwrap()));
    }
}
