
sample:	file format ELF32-arm-little

Disassembly of section .text:
00000000000000a8 main:
      a8:	sub	sp, #8
      aa:	bl	#188
      ae:	movs	r7, #1
      b0:	ands	r0, r7
      b2:	mov	r1, sp
      b4:	strb	r0, [r1]
      b6:	bl	#166
      ba:	bl	#186
      be:	ldr	r0, [pc, #124]
      c0:	ldrb	r0, [r0]
      c2:	cmp	r0, #0
      c4:	beq	#98 <main+0x82>
      c6:	movs	r4, #0
      c8:	mov	r0, sp
      ca:	bl	#162
      ce:	cmp	r0, #0
      d0:	beq	#2 <main+0x2e>
      d2:	bl	#142
      d6:	cmp	r4, #0
      d8:	beq	#42 <main+0x5e>
      da:	add	r4, sp, #4
      dc:	mov	r0, r4
      de:	bl	#106
      e2:	movs	r1, #113
      e4:	lsls	r1, r1, #4
      e6:	movs	r5, #3
      e8:	str	r5, [r0, r1]
      ea:	mov	r0, r4
      ec:	bl	#92
      f0:	ldr	r1, [pc, #76]
      f2:	str	r5, [r0, r1]
      f4:	mov	r0, r4
      f6:	bl	#82
      fa:	lsls	r1, r7, #13
      fc:	ldr	r2, [pc, #68]
      fe:	str	r1, [r0, r2]
     100:	movs	r6, #0
     102:	ldr	r5, [pc, #68]
     104:	b	#2 <main+0x62>
     106:	b	#-4 <main+0x5e>
     108:	mov	r7, r4
     10a:	adds	r6, r6, #1
     10c:	mov	r0, r6
     10e:	mov	r4, r7
     110:	ands	r0, r7
     112:	lsls	r7, r0, #13
     114:	add	r0, sp, #4
     116:	bl	#50
     11a:	ldr	r1, [pc, #40]
     11c:	str	r7, [r0, r1]
     11e:	mov	r7, r5
     120:	bl	#68
     124:	subs	r7, r7, #1
     126:	bne	#-10 <main+0x78>
     128:	b	#-36 <main+0x60>
     12a:	bl	#38
     12e:	mov	r4, r7
     130:	mov	r0, sp
     132:	bl	#58
     136:	cmp	r0, #0
     138:	bne	#-106 <main+0x2a>
     13a:	b	#-104 <main+0x2e>

000000000000013c $d.1:
     13c:	00 00 00 20 	.word	0x20000000
     140:	34 07 00 00 	.word	0x00000734
     144:	04 05 00 00 	.word	0x00000504
     148:	40 42 0f 00 	.word	0x000f4240

000000000000014c <nrf51::GPIO as core::ops::deref::Deref>::deref::h33ca7f3214a5ec9c:
     14c:	movs	r0, #5
     14e:	lsls	r0, r0, #28
     150:	bx	lr
     152:	bmi	#-88 <main+0x56>

0000000000000154 nrf51::Peripherals::steal::hf012edf585c054a0:
     154:	ldr	r0, [pc, #4]
     156:	movs	r1, #1
     158:	strb	r1, [r0]
     15a:	bx	lr

000000000000015c $d.110:
     15c:	00 00 00 20 	.word	0x20000000

0000000000000160 __cpsid:
     160:	cpsid i
     162:	bx	lr

0000000000000164 __cpsie:
     164:	cpsie i
     166:	bx	lr

0000000000000168 __nop:
     168:	bx	lr

000000000000016a __primask:
     16a:	mrs	r0, primask
     16e:	bx	lr

0000000000000170 cortex_m::register::primask::Primask::is_active::h0be98ab1298fa343:
     170:	ldrb	r1, [r0]
     172:	rsbs	r0, r1, #0
     174:	adcs	r0, r1
     176:	bx	lr

0000000000000178 bare_metal::CriticalSection::new::h4396954e5425e27a:
     178:	bx	lr
     17a:	bmi	#-88 <main+0x7e>

000000000000017c Reset:
     17c:	bl	#62
     180:	ldr	r0, [pc, #36]
     182:	ldr	r1, [pc, #40]
     184:	cmp	r1, r0
     186:	bhs	#6 <Reset+0x14>
     188:	movs	r2, #0
     18a:	stm	r1!, {r2}
     18c:	cmp	r1, r0
     18e:	blo	#-10 <Reset+0xc>
     190:	ldr	r0, [pc, #28]
     192:	ldr	r1, [pc, #32]
     194:	cmp	r1, r0
     196:	bhs	#8 <Reset+0x26>
     198:	ldr	r2, [pc, #28]
     19a:	ldm	r2!, {r3}
     19c:	stm	r1!, {r3}
     19e:	cmp	r1, r0
     1a0:	blo	#-10 <Reset+0x1e>
     1a2:	bl	#-254
     1a6:	trap

00000000000001a8 $d.6:
     1a8:	04 00 00 20 	.word	0x20000004
     1ac:	00 00 00 20 	.word	0x20000000
     1b0:	00 00 00 20 	.word	0x20000000
     1b4:	00 00 00 20 	.word	0x20000000
     1b8:	d8 01 00 00 	.word	0x000001d8

00000000000001bc DefaultHandler_:
     1bc:	b	#-4 <DefaultHandler_>

00000000000001bd WDT:
     1bd:	strb	r7, [r4, #3]

00000000000001be DefaultPreInit:
     1be:	bx	lr

00000000000001bf __pre_init:
     1bf:	strb	r7, [r0, #1]

00000000000001c0 HardFaultTrampoline:
     1c0:	mov	r0, lr
     1c2:	movs	r1, #4
     1c4:	tst	r0, r1
     1c6:	bne	#4 <HardFaultTrampoline+0xe>
     1c8:	mrs	r0, msp
     1cc:	b	#4 <HardFault_>
     1ce:	mrs	r0, psp
     1d2:	b	#-2 <HardFault_>

00000000000001d4 HardFault_:
     1d4:	b	#-4 <HardFault_>

00000000000001d5 HardFault:
     1d5: <unknown>
