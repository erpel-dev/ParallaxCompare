#include "ecu.h"
static const unsigned short ignition_map[8][8] = { {1, 2, 3, 4, 5, 6, 7, 8} };
static int integrator;
unsigned int control_step(unsigned int state, int sensor)
{
    integrator += sensor - 512;
#ifdef BIGGER_CONTROL
    if (integrator > 100000) integrator = 100000;
    if (integrator < -100000) integrator = -100000;
    state ^= (unsigned int)ignition_map[sensor & 7][state & 7] << 3;
#endif
    return state + ignition_map[state & 7][sensor & 7] + (integrator >> 4);
}
int sensor_read(int channel) { return 400 + channel * 37; }
