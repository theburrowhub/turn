# Phosphor Icons

`Phosphor-Regular.ttf` is the regular weight of [Phosphor Icons](https://phosphoricons.com/),
copyright (c) 2023 Phosphor Icons, used under the MIT licence:

> Permission is hereby granted, free of charge, to any person obtaining a copy of this software
> and associated documentation files (the "Software"), to deal in the Software without
> restriction, including without limitation the rights to use, copy, modify, merge, publish,
> distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the
> Software is furnished to do so, subject to the following conditions:
>
> The above copyright notice and this permission notice shall be included in all copies or
> substantial portions of the Software.
>
> THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING
> BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
> NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM,
> DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
> OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

## Why the file is here rather than in a dependency

Turn used the `egui-phosphor` crate, which bundles this same font and generates one constant per
icon. It pins `egui = "0.35"`, which cargo reads as "not 0.36" — so a crate whose entire job is
to hand over 480 kB of font and a list of codepoints decided which version of egui Turn could
build against, and would go on deciding it at every future egui release.

The font is vendored here and the codepoints Turn actually draws are declared in `src/icons.rs`.
Adding an icon means looking its codepoint up at phosphoricons.com and writing one line, which is
a smaller price than a release schedule that is not ours.
