//! Contains helpers to build instructions for debugger use
pub(crate) mod aarch32 {
    /// Build a MOV instruction
    pub(crate) fn build_mov(rd: u16, rm: u16) -> u32 {
        let mut ret = 0b1110_0001_1010_0000_0000_0000_0000_0000;

        ret |= (rd as u32) << 12;
        ret |= rm as u32;

        ret
    }

    /// Build a MCR instruction
    pub(crate) fn build_mcr(
        coproc: u8,
        opcode1: u8,
        reg: u16,
        ctrl_reg_n: u8,
        ctrl_reg_m: u8,
        opcode2: u8,
    ) -> u32 {
        let mut ret = 0b1110_1110_0000_0000_0000_0000_0001_0000;

        ret |= (coproc as u32) << 8;
        ret |= (opcode1 as u32) << 21;
        ret |= (reg as u32) << 12;
        ret |= (ctrl_reg_n as u32) << 16;
        ret |= ctrl_reg_m as u32;
        ret |= (opcode2 as u32) << 5;

        ret
    }

    pub(crate) fn build_mrc(
        coproc: u8,
        opcode1: u8,
        reg: u16,
        ctrl_reg_n: u8,
        ctrl_reg_m: u8,
        opcode2: u8,
    ) -> u32 {
        let mut ret = 0b1110_1110_0001_0000_0000_0000_0001_0000;

        ret |= (coproc as u32) << 8;
        ret |= (opcode1 as u32) << 21;
        ret |= (reg as u32) << 12;
        ret |= (ctrl_reg_n as u32) << 16;
        ret |= ctrl_reg_m as u32;
        ret |= (opcode2 as u32) << 5;

        ret
    }

    pub(crate) fn build_bx(reg: u16) -> u32 {
        let mut ret = 0b1110_0001_0010_1111_1111_1111_0001_0000;

        ret |= reg as u32;

        ret
    }

    pub(crate) fn build_ldc(coproc: u8, ctrl_reg: u8, reg: u16, imm: u8) -> u32 {
        let mut ret = 0b1110_1100_1011_0000_0000_0000_0000_0000;

        ret |= (reg as u32) << 16;
        ret |= (ctrl_reg as u32) << 12;
        ret |= (coproc as u32) << 8;
        ret |= (imm as u32) >> 2;

        ret
    }

    pub(crate) fn build_stc(coproc: u8, ctrl_reg: u8, reg: u16, imm: u8) -> u32 {
        let mut ret = 0b1110_1100_1010_0000_0000_0000_0000_0000;

        ret |= (reg as u32) << 16;
        ret |= (ctrl_reg as u32) << 12;
        ret |= (coproc as u32) << 8;
        ret |= (imm as u32) >> 2;

        ret
    }

    pub(crate) fn build_mrs(reg: u16) -> u32 {
        let mut ret = 0b1110_0001_0000_1111_0000_0000_0000_0000;

        ret |= (reg as u32) << 12;

        ret
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn gen_mcr_instruction() {
            let instr = build_mcr(14, 0, 2, 1, 2, 3);

            // MCR p14, 0, r2, c1, c2, 3
            assert_eq!(0xEE012E72, instr);
        }

        #[test]
        fn gen_mrc_instruction() {
            let instr = build_mrc(14, 0, 2, 1, 2, 3);

            // MRC p14, 0, r2, c1, c2, 3
            assert_eq!(0xEE112E72, instr);
        }

        #[test]
        fn gen_mov_instruction() {
            let instr = build_mov(2, 15);

            // MOV r2, pc
            assert_eq!(0xE1A0200F, instr);
        }

        #[test]
        fn gen_bx_instruction() {
            let instr = build_bx(2);

            // BX r2
            assert_eq!(0xE12FFF12, instr);
        }

        #[test]
        fn gen_ldc_instruction() {
            let instr = build_ldc(14, 5, 2, 4);

            // LDC p14, c5, [r2], #4
            assert_eq!(0xECB25E01, instr);
        }

        #[test]
        fn gen_stc_instruction() {
            let instr = build_stc(14, 5, 2, 4);

            // STC p14, c5, [r2], #4
            assert_eq!(0xECA25E01, instr);
        }

        #[test]
        fn gen_mrs_instruction() {
            let instr = build_mrs(2);

            // MRS r2, CPSR
            assert_eq!(0xE10F2000, instr);
        }
    }
}

pub(crate) mod thumb2 {
    // These are the same encoding in thumb2
    pub(crate) use super::aarch32::{build_mcr, build_mrc};

    pub(crate) fn build_ldr(reg_target: u16, reg_source: u16, imm: u8) -> u32 {
        let mut ret = 0b1111_1000_0101_0000_0000_1011_0000_0000;

        ret |= (reg_source as u32) << 16;
        ret |= (reg_target as u32) << 12;
        ret |= imm as u32;

        ret
    }

    pub(crate) fn build_str(reg_target: u16, reg_source: u16, imm: u8) -> u32 {
        let mut ret = 0b1111_1000_0100_0000_0000_1011_0000_0000;

        ret |= (reg_source as u32) << 16;
        ret |= (reg_target as u32) << 12;
        ret |= imm as u32;

        ret
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn gen_ldr_instruction() {
            let instr = build_ldr(2, 3, 4);

            // LDR r2, [r3], #4
            assert_eq!(0xF8532B04, instr);
        }

        #[test]
        fn gen_str_instruction() {
            let instr = build_str(2, 3, 4);

            // STR r2, [r3], #4
            assert_eq!(0xF8432B04, instr);
        }
    }
}
