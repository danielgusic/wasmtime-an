;; Microbenchmark for the aligned, full-width i32.load path.
;;
;; Pass an aligned address (0 is initialized to 1). The load contributes to the
;; returned sum, which keeps both the load and loop observable. Keeping the
;; address as a parameter also preserves the runtime alignment check.
(module
  (memory (export "memory") 1)
  (data (i32.const 0) "\01\00\00\00")

  (func (export "bench")
    (param $iterations i32)
    (param $address i32)
    (result i32)
    (local $sum i32)
    (local $i i32)

    (loop $loop
      local.get $sum
      local.get $address
      i32.load align=4
      i32.add
      local.set $sum

      local.get $i
      i32.const 1
      i32.add
      local.tee $i
      local.get $iterations
      i32.lt_u
      br_if $loop)

    local.get $sum))
