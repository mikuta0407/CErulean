package arm

import "testing"

func TestDisasm(t *testing.T) {
	// 期待文字列は本実装の表示形式（GNU as 風だが厳密互換は目的でない）。
	tests := []struct {
		word uint32
		pc   uint32
		want string
	}{
		{0xE3A0D901, 0, "mov sp, #0x4000"},           // MOV sp, #0x4000
		{0xE1A01002, 0, "mov r1, r2"},                // MOV r1, r2
		{0xE0813282, 0, "add r3, r1, r2, lsl #5"},    // ADD r3, r1, r2, lsl #5
		{0xE2522001, 0, "subs r2, r2, #0x1"},         // SUBS r2, r2, #1
		{0xE3530000, 0, "cmp r3, #0x0"},              // CMP r3, #0
		{0x0A000003, 0x1000, "beq 0x00001014"},       // BEQ +
		{0xEB000000, 0x1000, "bl 0x00001008"},        // BL
		{0xE12FFF11, 0, "bx r1"},                     // BX r1
		{0xE5912004, 0, "ldr r2, [r1, #0x4]"},        // LDR r2, [r1, #4]
		{0xE5A12004, 0, "str r2, [r1, #0x4]!"},       // STR r2, [r1, #4]!
		{0xE4912004, 0, "ldr r2, [r1], #0x4"},        // LDR r2, [r1], #4
		{0xE5D12000, 0, "ldrb r2, [r1]"},             // LDRB r2, [r1]
		{0xE7912002, 0, "ldr r2, [r1, r2]"},          // LDR r2, [r1, r2]
		{0xE1D120B4, 0, "ldrh r2, [r1, #0x4]"},       // LDRH r2, [r1, #4]
		{0xE8BD8010, 0, "ldmia sp!, {r4,pc}"},        // LDMIA sp!, {r4, pc}
		{0xE92D4010, 0, "stmdb sp!, {r4,lr}"},        // STMDB sp!, {r4, lr}
		{0xE10F1000, 0, "mrs r1, cpsr"},              // MRS r1, CPSR
		{0xE129F001, 0, "msr cpsr_cf, r1"},           // MSR CPSR_fc, r1
		{0xE0030291, 0, "mul r3, r1, r2"},            // MUL r3, r1, r2
		{0xE0854392, 0, "umull r4, r5, r2, r3"},      // UMULL r4, r5, r2, r3
		{0xE1013092, 0, "swp r3, r2, [r1]"},          // SWP r3, r2, [r1]
		{0xEE110F10, 0, "mrc p15, 0, r0, c1, c0, 0"}, // MRC p15,0,r0,c1,c0,0
		{0xEE010F10, 0, "mcr p15, 0, r0, c1, c0, 0"}, // MCR p15,0,r0,c1,c0,0
		{0xEF000010, 0, "swi 0x000010"},              // SWI 0x10
		{0xE7F000F0, 0, ".word 0xE7F000F0"},          // 未定義空間
	}
	for _, tt := range tests {
		if got := Disasm(tt.word, tt.pc); got != tt.want {
			t.Errorf("Disasm(%08X) = %q, want %q", tt.word, got, tt.want)
		}
	}
}

func TestDisasmThumb(t *testing.T) {
	// 期待値のエンコードは ARM ARM Chapter A7 の各形式から手で組んだもの。
	tests := []struct {
		hw, next, pc uint32
		want         string
	}{
		{0x2005, 0, 0, "movs r0, #0x5"},                         // Format 3
		{0x1888, 0, 0, "adds r0, r1, r2"},                       // Format 2 レジスタ
		{0x1E48, 0, 0, "subs r0, r1, #1"},                       // Format 2 即値
		{0x0048, 0, 0, "lsls r0, r1, #1"},                       // Format 1
		{0x4288, 0, 0, "cmp r0, r1"},                            // Format 4（フラグのみ）
		{0x4008, 0, 0, "ands r0, r1"},                           // Format 4
		{0x4770, 0, 0, "bx lr"},                                 // Format 5
		{0x46C0, 0, 0, "mov r8, r8"},                            // Format 5 (NOP 慣用)
		{0x4801, 0, 0x1002, "ldr r0, [pc, #0x4] ; =0x00001008"}, // Format 6（PC はワードアライン）
		{0x5888, 0, 0, "ldr r0, [r1, r2]"},                      // Format 7
		{0x6848, 0, 0, "ldr r0, [r1, #0x4]"},                    // Format 9（imm5*4）
		{0x7848, 0, 0, "ldrb r0, [r1, #0x1]"},                   // Format 9 バイト
		{0x8848, 0, 0, "ldrh r0, [r1, #0x2]"},                   // Format 10
		{0x9801, 0, 0, "ldr r0, [sp, #0x4]"},                    // Format 11
		{0xA801, 0, 0, "add r0, sp, #0x4"},                      // Format 12
		{0xB082, 0, 0, "sub sp, #0x8"},                          // Format 13
		{0xB510, 0, 0, "push {r4,lr}"},                          // Format 14
		{0xBD10, 0, 0, "pop {r4,pc}"},                           // Format 14
		{0xC103, 0, 0, "stmia r1!, {r0,r1}"},                    // Format 15
		{0xD0FE, 0, 0x1000, "beq 0x00001000"},                   // Format 16（自分自身へ）
		{0xDF01, 0, 0, "swi 0x01"},                              // Format 17
		{0xDE00, 0, 0, ".hword 0xDE00"},                         // 未定義
		{0xE7FE, 0, 0x1000, "b 0x00001000"},                     // Format 18
		{0xF000, 0xF802, 0x1000, "bl 0x00001008"},               // Format 19（2 ハーフワード）
		{0xF802, 0, 0x1002, "bl.suffix lr+0x4"},                 // サフィックス単体
	}
	for _, tt := range tests {
		if got := DisasmThumb(tt.hw, tt.next, tt.pc); got != tt.want {
			t.Errorf("DisasmThumb(%04X) = %q, want %q", tt.hw, got, tt.want)
		}
	}
}
