    .global test_func
    .thumb_func
test_func:
    mov     r0, #0          /* 0x00 */
    mov     r1, #128        /* 0x02 */
    mov     r2, #0          /* 0x04 */
    bkpt                    /* 0x06 */
loop:
    add     r0, r0, #1      /* 0x08 */
    cmp     r0, r1          /* 0x0a */
    ble     loop            /* 0x0c */ 

    mov     r2, #1          /* 0x0e */
    bkpt                    /* 0x10 */
finish:
    b finish                /* 0x12 */






