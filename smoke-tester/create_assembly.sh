arm-none-eabi-as test_function_arm.s -o test_arm.o
arm-none-eabi-objcopy test_arm.o -O binary test_arm.bin

mv test_arm.bin src/tests/