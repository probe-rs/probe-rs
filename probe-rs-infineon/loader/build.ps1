# Builds the TC27x PFLASH loader with the AURIX GCC (tricore-elf-gcc):
#   m25_flash_loader_tc27x.bin
# The binary is committed because the probe-rs build has no TriCore toolchain.
# After a rebuild, update the loader check in src/flash.rs (size and SHA-256).
param(
    [string]$Toolchain = 'C:\Infineon\AURIX-Configuration-Studio-1.0.24\eclipse\tricore-gcc11\bin'
)
$ErrorActionPreference = 'Stop'
Push-Location $PSScriptRoot
try {
    $n = 'm25_flash_loader_tc27x'
    & "$Toolchain\tricore-elf-gcc.exe" -c -O2 -nostdlib -ffreestanding -mcpu=tc27xx -DTC27X -o "$n.o" m25_flash_loader.c
    if ($LASTEXITCODE) { throw "compile failed" }
    & "$Toolchain\tricore-elf-gcc.exe" -nostdlib -mcpu=tc27xx -T m25_flash_loader.ld -o "$n.elf" "$n.o"
    if ($LASTEXITCODE) { throw "link failed" }
    & "$Toolchain\tricore-elf-objcopy.exe" -O binary "$n.elf" "$n.bin"
    if ($LASTEXITCODE) { throw "objcopy failed" }
    Remove-Item "$n.o", "$n.elf"
    Get-FileHash "$n.bin" -Algorithm SHA256 | Select-Object Hash
}
finally {
    Pop-Location
}
