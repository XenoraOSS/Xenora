// SPDX-License-Identifier: GPL-3.0-or-later
// The Microsoft x64 floating-point helper the compiler references when a
// translation unit holds floating-point constants. The acceptance program
// never calls it; the loader never runs it either.
int _fltused(void) { return 0; }
