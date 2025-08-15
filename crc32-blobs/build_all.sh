#!/bin/bash
# 
# CRC32 Algorithm Build Script for probe-rs
# Builds optimized slicing-by-4 CRC32 implementation for ARM Thumb
#

set -e  # Exit on any error

# Color codes for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
WHITE='\033[1;37m'
NC='\033[0m' # No Color

# Build configuration
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

print_header() {
    echo -e "${WHITE}=======================================${NC}"
    echo -e "${WHITE}  probe-rs CRC32 Algorithm Builder${NC}"
    echo -e "${WHITE}=======================================${NC}"
    echo ""
    echo -e "${BLUE}Building optimized slicing-by-4 CRC32 for ARM Thumb${NC}"
    echo ""
}

check_dependencies() {
    echo -e "${BLUE}Checking build dependencies...${NC}"
    
    local missing_deps=()
    
    # Check for required tools
    for tool in make arm-none-eabi-gcc arm-none-eabi-objcopy arm-none-eabi-objdump; do
        if ! command -v "$tool" &> /dev/null; then
            missing_deps+=("$tool")
        fi
    done
    
    if [[ ${#missing_deps[@]} -gt 0 ]]; then
        echo -e "${RED}Missing dependencies:${NC}"
        for dep in "${missing_deps[@]}"; do
            echo "  - $dep"
        done
        echo ""
        echo -e "${YELLOW}Install missing dependencies:${NC}"
        echo "  sudo apt install gcc-arm-none-eabi binutils-arm-none-eabi"
        echo ""
        exit 1
    fi
    
    echo -e "${GREEN}✓ All dependencies satisfied${NC}"
    echo ""
}

build_arm_thumb() {
    local arch_dir="$SCRIPT_DIR/arm-thumb"
    echo -e "${BLUE}Building ARM Thumb slicing-by-4 CRC32...${NC}"
    
    if [[ ! -d "$arch_dir" ]]; then
        echo -e "${RED}Error: ARM Thumb directory not found: $arch_dir${NC}"
        return 1
    fi
    
    cd "$arch_dir"
    
    # Build optimized slicing-by-4 implementation
    echo "  Building slicing-by-4 optimized CRC32..."
    if make performance; then
        echo -e "    ${GREEN}✓ Slicing-by-4 CRC32 built successfully${NC}"
        local size=$(stat -c%s crc32_slice4_optimized.bin)
        echo "      Binary size: $size bytes"
    else
        echo -e "    ${RED}✗ Slicing-by-4 CRC32 build failed${NC}"
        return 1
    fi
    
    echo -e "${GREEN}✓ ARM Thumb build completed${NC}"
    echo ""
}

validate_build() {
    echo -e "${BLUE}Validating build artifacts...${NC}"
    
    cd "$SCRIPT_DIR/arm-thumb"
    
    # Check essential binary
    if [[ -f "crc32_slice4_optimized.bin" ]]; then
        local size=$(stat -c%s "crc32_slice4_optimized.bin")
        if [[ $size -lt 1000 || $size -gt 10000 ]]; then
            echo -e "  ${YELLOW}⚠ Suspicious size for binary: $size bytes${NC}"
            return 1
        fi
    else
        echo -e "  ${RED}✗ Missing essential binary: crc32_slice4_optimized.bin${NC}"
        return 1
    fi
    
    # Check for symbols
    if [[ -f "crc32_slice4_optimized.elf" ]]; then
        if arm-none-eabi-nm crc32_slice4_optimized.elf | grep -q "calculate_crc32"; then
            echo -e "  ${GREEN}✓ ARM Thumb symbols verified${NC}"
        else
            echo -e "  ${RED}✗ Missing required symbols${NC}"
            return 1
        fi
    fi
    
    echo -e "${GREEN}✓ Build validated successfully${NC}"
    return 0
}

main() {
    print_header
    check_dependencies
    
    # Build the slicing-by-4 algorithm
    if ! build_arm_thumb; then
        echo -e "${RED}✗ Build failed${NC}"
        exit 1
    fi
    
    # Validate build
    if ! validate_build; then
        echo -e "${RED}✗ Build validation failed${NC}"
        exit 1
    fi
    
    # Final status
    echo -e "${WHITE}=======================================${NC}"
    echo -e "${GREEN}✓ Build completed successfully!${NC}"
    echo ""
    echo -e "${BLUE}Output:${NC}"
    echo "  Binary: arm-thumb/crc32_slice4_optimized.bin"
    echo "  Size: $(stat -c%s "$SCRIPT_DIR/arm-thumb/crc32_slice4_optimized.bin") bytes"
    echo ""
    echo -e "${BLUE}Expected Performance (RP2040 @ 125MHz):${NC}"
    echo "  Speed: ~600-800 KB/s (6-8x improvement)"
    echo "  4KB sector: 3-5ms (vs 25ms baseline)"
    echo ""
}

# Handle Ctrl+C gracefully
trap 'echo -e "\n${YELLOW}Build interrupted by user${NC}"; exit 130' INT

# Run main function
main "$@"