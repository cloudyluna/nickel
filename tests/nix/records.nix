# has_field operator (`?`)
[
  ({ a = 1; } ? a == true)
  ({ a = 1; } ? "a" == true)
  ({ a = 1; } ? b == false)
  ({ a = 1; } ? "b" == false)
  ({ a.foo = 1; } ? a.foo == true)
  ({ a.foo = 1; } ? a."foo" == true)
  ({ a.foo = 1; } ? "a.foo" == false)
  ({ a.foo = 1; } ? "a".foo == true)
  ({ a.foo = 1; } ? a == true)
]