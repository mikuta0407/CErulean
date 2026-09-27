package arm

import "testing"

func TestDisasm(t *testing.T) {
	// 期待文字列は本実装の表示形式（GNU as 風だが厳密互換は目的でない）。
	tests := []struct {
		word uint32
		pc   uint32
		want string
	}{
		{0xE3A0D901, 0, "mov sp, #0x4000"},                  // MOV sp, #0x4000
		{0xE1A01002, 0, "mov r1, r2"},                       // MOV r1, r2
		{0xE0813282, 0, "add r3, r1, r2, lsl #5"},           // ADD r3, r1, r2, lsl #5
		{0xE2522001, 0, "subs r2, r2, #0x1"},                // SUBS r2, r2, #1
		{0xE3530000, 0, "cmp r3, #0x0"},                     // CMP r3, #0
		{0x0A000003, 0x1000, "beq 0x00001014"},              // BEQ +
		{0xEB000000, 0x1000, "bl 0x00001008"},               // BL
		{0xE12FFF11, 0, "bx r1"},                            // BX r1
		{0xE5912004, 0, "ldr r2, [r1, #0x4]"},               // LDR r2, [r1, #4]
		{0xE5A12004, 0, "str r2, [r1, #0x4]!"},              // STR r2, [r1, #4]!
		{0xE4912004, 0, "ldr r2, [r1], #0x4"},               // LDR r2, [r1], #4
		{0xE5D12000, 0, "ldrb r2, [r1]"},                    // LDRB r2, [r1]
		{0xE7912002, 0, "ldr r2, [r1, r2]"},                 // LDR r2, [r1, r2]
		{0xE1D120B4, 0, "ldrh r2, [r1, #0x4]"},              // LDRH r2, [r1, #4]
		{0xE8BD8010, 0, "ldmia sp!, {r4,pc}"},               // LDMIA sp!, {r4, pc}
		{0xE92D4010, 0, "stmdb sp!, {r4,lr}"},               // STMDB sp!, {r4, lr}
		{0xE10F1000, 0, "mrs r1, cpsr"},                     // MRS r1, CPSR
		{0xE129F001, 0, "msr cpsr_cf, r1"},                  // MSR CPSR_fc, r1
		{0xE0030291, 0, "mul r3, r1, r2"},                   // MUL r3, r1, r2
		{0xE0854392, 0, "umull r4, r5, r2, r3"},             // UMULL r4, r5, r2, r3
		{0xE1013092, 0, "swp r3, r2, [r1]"},                 // SWP r3, r2, [r1]
		{0xEE110F10, 0, "mrc p15, 0, r0, c1, c0, 0"},        // MRC p15,0,r0,c1,c0,0
		{0xEE010F10, 0, "mcr p15, 0, r0, c1, c0, 0"},        // MCR p15,0,r0,c1,c0,0
		{0xEF000010, 0, "swi 0x000010"},                     // SWI 0x10
		{0xE7F000F0, 0, ".word 0xE7F000F0"},                 // 未定義空間
	}
	for _, tt := range tests {
		if got := Disasm(tt.word, tt.pc); got != tt.want {
			t.Errorf("Disasm(%08X) = %q, want %q", tt.word, got, tt.want)
		}
	}
}
