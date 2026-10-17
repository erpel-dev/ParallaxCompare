#include "sensors.h"
struct calibration calibration = { { 1, 2, 3, 4 }, -5, 1, 0.5 };
enum mode mode = MODE_RUN;
const char *channel_names[4] = { "speed", "temp", "load", "lambda" };
__thread int tls_errors;
static uint32_t counter;
static int16_t history[HISTORY][4];
#ifdef DIAG
int diag_level = 2;
#endif

static int smooth(int v) { return (v + history[counter % HISTORY][0]) / 2; }

int read_sensor(int channel)
{
    static int calls;
    calls++;
    counter++;
    int v = smooth(channel * calibration.gain[channel & 3] + calibration.offset);
    history[counter % HISTORY][channel & 3] = (int16_t)v;
    if (v < 0) tls_errors++;
    return v;
}
