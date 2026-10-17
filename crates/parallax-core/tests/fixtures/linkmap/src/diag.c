#include "ecu.h"
static unsigned int diag_requests;
const char diag_version[] = "DIAG 2.0";
void diag_poll(void) { diag_requests++; }
