(component
  (core module $m
    (func $add_two (param i32) (result i32)
      local.get 0
      i32.const 2
      i32.add
    )
    (export "add-two" (func $add_two))
  )
  (core instance $i (instantiate $m))
  (func $f (param "x" s32) (result s32) (canon lift (core func $i "add-two")))
  (export "add-two" (func $f))
)
