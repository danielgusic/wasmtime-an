(module
  (import "wasi_snapshot_preview1" "fd_read"
    (func $fd_read (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_write"
    (func $fd_write (param i32 i32 i32 i32) (result i32)))

  (memory (export "memory") 1)

  (func $parse (param $len i32) (result i32)
    (local $i i32)
    (local $n i32)
    (local $c i32)
    (local.set $i (i32.const 0))
    (local.set $n (i32.const 0))
    (block $done
      (loop $l
        (br_if $done (i32.ge_u (local.get $i) (local.get $len)))
        (local.set $c (i32.load8_u (i32.add (i32.const 64) (local.get $i))))
        (br_if $done (i32.lt_u (local.get $c) (i32.const 48)))
        (br_if $done (i32.gt_u (local.get $c) (i32.const 57)))
        (local.set $n
          (i32.add
            (i32.mul (local.get $n) (i32.const 10))
            (i32.sub (local.get $c) (i32.const 48))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $l)))
    (local.get $n))

  (func $fib (param $n i32) (result i32)
    (local $a i32)
    (local $b i32)
    (local $t i32)
    (local $i i32)
    (local.set $a (i32.const 0))
    (local.set $b (i32.const 1))
    (local.set $i (i32.const 0))
    (block $done
      (loop $l
        (br_if $done (i32.ge_u (local.get $i) (local.get $n)))
        (local.set $t (i32.add (local.get $a) (local.get $b)))
        (local.set $a (local.get $b))
        (local.set $b (local.get $t))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $l)))
    (local.get $a))

  (func $format (param $n i32) (result i32 i32)
    (local $p i32)
    (local $end i32)
    (local.set $end (i32.const 160))
    (local.set $p (local.get $end))
    (if (i32.eqz (local.get $n))
      (then
        (local.set $p (i32.sub (local.get $p) (i32.const 1)))
        (i32.store8 (local.get $p) (i32.const 48))))
    (block $done
      (loop $l
        (br_if $done (i32.eqz (local.get $n)))
        (local.set $p (i32.sub (local.get $p) (i32.const 1)))
        (i32.store8 (local.get $p)
          (i32.add (i32.const 48) (i32.rem_u (local.get $n) (i32.const 10))))
        (local.set $n (i32.div_u (local.get $n) (i32.const 10)))
        (br $l)))
    (i32.store8 (local.get $end) (i32.const 10))
    (local.get $p)
    (i32.add (i32.sub (local.get $end) (local.get $p)) (i32.const 1)))

  (func (export "_start")
    (local $nread i32)
    (local $n i32)
    (local $result i32)
    (local $ptr i32)
    (local $len i32)

    (i32.store (i32.const 0) (i32.const 64))
    (i32.store (i32.const 4) (i32.const 32))

    (drop (call $fd_read
      (i32.const 0) (i32.const 0) (i32.const 1) (i32.const 8)))
    (local.set $nread (i32.load (i32.const 8)))

    (local.set $n (call $parse (local.get $nread)))
    (local.set $result (call $fib (local.get $n)))

    (call $format (local.get $result))
    (local.set $len)
    (local.set $ptr)

    (i32.store (i32.const 16) (local.get $ptr))
    (i32.store (i32.const 20) (local.get $len))

    (drop (call $fd_write
      (i32.const 1) (i32.const 16) (i32.const 1) (i32.const 24))))
)
