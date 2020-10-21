onizd is the server component of [ZTransport][1], a mod for Oxygen Not Included.

[1]: https://github.com/BloodyRum/ZTransport

# Windows Usage

If you're on Windows, all you need is to go straight to [the Releases page](https://github.com/SolraBizna/onizd/releases) and download a prebuilt, graphical Windows version.

# Other OSes (Linux, macOS...)

## Compiling

Install Rust, if you don't have it already. Installing Rust is pretty simple if you follow [these directions][2].

Clone this repository (or otherwise get the source code):

```sh
git clone https://github.com/SolraBizna/onizd
```

[2]: https://www.rust-lang.org/learn/get-started

## Running

If you want to run the GUI version:

```sh
cd onizd
cargo run --release --features gui
```

Chances are the GUI version is good enough for your needs. But, if you want the command line version instead:

```sh
cd onizd
cargo run --release --
```

Any additional command line arguments you want go after the `--`. Pass `--help` after the `--` to see a list of command line arguments.

Example:

```sh
cargo run --release -- -v
```

This example will print some information every time something passes (or tries to pass) the Z barrier, many times per second.

# Legalese

onizd is copyright ©2020 Solra Bizna. If you submit improvements to onizd in the form of Pull Requests via GitHub, it is assumed that you are assigning copyright on your improvements to Solra Bizna, unless you clearly and explicitly state otherwise *before* your Pull Request is merged.

onizd is free software: you can redistribute it and/or modify it under the terms of the GNU General Public License as published by the Free Software Foundation, either version 3 of the License, or (at your option) any later version.

onizd is distributed in the hope that it will be useful, but WITHOUT ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the GNU General Public License for more details.

You should have received [a copy of the GNU General Public License](COPYING.md) along with onizd. If not, see <https://www.gnu.org/licenses/>.
