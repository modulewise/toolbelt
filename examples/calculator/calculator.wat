(component
  (core module $m
    (func (export "add") (param i32 i32) (result i32)
      local.get 0
      local.get 1
      i32.add
    )
    (func (export "sub") (param i32 i32) (result i32)
      local.get 0
      local.get 1
      i32.sub
    )
    (func (export "mul") (param i32 i32) (result i32)
      local.get 0
      local.get 1
      i32.mul
    )
    (func (export "div") (param i32 i32) (result i32)
      local.get 0
      local.get 1
      i32.div_s
    )
  )
  (core instance $i (instantiate $m))
  (func (export "add")
    (param "a" s32) (param "b" s32) (result s32)
    (canon lift (core func $i "add"))
  )
  (func (export "subtract")
    (param "a" s32) (param "b" s32) (result s32)
    (canon lift (core func $i "sub"))
  )
  (func (export "multiply")
    (param "a" s32) (param "b" s32) (result s32)
    (canon lift (core func $i "mul"))
  )
  (func (export "divide")
    (param "a" s32) (param "b" s32) (result s32)
    (canon lift (core func $i "div"))
  )
)
