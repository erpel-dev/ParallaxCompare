#include "ecu.h"
extern unsigned long _sdata, _edata, _sbss, _ebss;
__attribute__((section(".isr_vector"))) const void *vectors[4] = { (void *)0x20010000, reset_handler, 0, 0 };
volatile unsigned int tick_count;
unsigned int engine_state = 1;
__attribute__((section(".backup"))) unsigned int reset_reason;

void reset_handler(void)
{
    for (unsigned long *p = &_sbss; p < &_ebss; p++) *p = 0;
    can_init(500);
    if (reset_reason == 0xDEAD) tick_count = 100;
    for (;;) {
        tick_count++;
        engine_state = control_step(engine_state, sensor_read(tick_count & 7));
#ifdef WITH_DIAG
        diag_poll();
#endif
    }
}
