#include "sensors.h"
static union raw { uint32_t word; uint8_t bytes[4]; } raw;
static int (*filter)(int) = read_sensor;
int main(int argc, char **argv)
{
    (void)argv;
    raw.word = (uint32_t)filter(argc);
    return raw.bytes[0] + mode;
}
