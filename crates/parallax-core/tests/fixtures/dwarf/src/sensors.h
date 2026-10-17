#include <stdint.h>
#ifndef HISTORY
#define HISTORY 8
#endif
enum mode { MODE_OFF, MODE_RUN, MODE_LIMP };
struct calibration { int16_t gain[4]; int32_t offset; uint8_t flags; double scale; };
extern struct calibration calibration;
extern enum mode mode;
extern const char *channel_names[4];
extern __thread int tls_errors;
int read_sensor(int channel);
