; SPDX-License-Identifier: GPL-3.0-or-later
; Target-independent integer cross-check for issue #982.
; The first parameter is the synthetic lane number. It is intentionally unused;
; preserving that argument position makes the gfx1030 function-calling mapping
; place the input word in v1 while v0 remains the synthetic lane index.
define internal noundef i32 @xcheck_core(i32 noundef %lane, i32 noundef %x) alwaysinline {
entry:
  %mask1 = and i32 %x, 16777215
  %mul1 = mul i32 %mask1, 1193046
  %xor1 = xor i32 %mul1, %x
  %mask2 = and i32 %xor1, 16777215
  %mul2 = mul i32 %mask2, 1414220
  %xor2 = xor i32 %mul2, %xor1
  ret i32 %xor2
}

define noundef i32 @xcheck_arm64(i32 noundef %lane, i32 noundef %x) {
entry:
  %result = call i32 @xcheck_core(i32 %lane, i32 %x)
  ret i32 %result
}

define noundef i32 @xcheck_gfx1030(i32 noundef %lane, i32 noundef %x) {
entry:
  %result = call i32 @xcheck_core(i32 %lane, i32 %x)
  ret i32 %result
}
