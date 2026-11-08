# Pitot

## Third-Party Notices

Code or logic ported from other projects is listed here with its original license notice. Each entry is added
in the same commit as the code it covers. Licenses of Rust crate dependencies are collected separately for
release builds.

### iRacing SDK (`irsdk_defines.h`)

- Source: iRacing telemetry SDK, distributed by iRacing.com Motorsport Simulations
- License: BSD-3-Clause
- Used in: `crates/irsdk/src/layout.rs`
- Ported: the shared-memory and `.ibt` structure layouts (header, variable header, buffer descriptor, disk
  sub-header), variable type codes and sizes, and the related constants, re-expressed as Rust types with
  byte-level decoding

```text
Copyright (c) 2013, iRacing.com Motorsport Simulations, LLC.
All rights reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:
    * Redistributions of source code must retain the above copyright
      notice, this list of conditions and the following disclaimer.
    * Redistributions in binary form must reproduce the above copyright
      notice, this list of conditions and the following disclaimer in the
      documentation and/or other materials provided with the distribution.
    * Neither the name of iRacing.com Motorsport Simulations nor the
      names of its contributors may be used to endorse or promote products
      derived from this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND
ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED
WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT OWNER OR CONTRIBUTORS BE LIABLE FOR
ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES
(INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES;
LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND
ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
(INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS
SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
```

<!--
Entry template:

### <Project name>

- Source: <repository URL>
- License: <SPDX identifier>
- Used in: <crate and module paths>
- Ported: <what was ported or adapted, and how closely>

```text
<Original copyright notice and license text>
```
-->
