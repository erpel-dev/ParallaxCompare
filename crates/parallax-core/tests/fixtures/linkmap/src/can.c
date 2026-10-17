#include "ecu.h"
static unsigned char rx_buffer[64];
unsigned int can_bitrate;
void can_init(unsigned int kbit) { can_bitrate = kbit * 1000; rx_buffer[0] = 0; }
