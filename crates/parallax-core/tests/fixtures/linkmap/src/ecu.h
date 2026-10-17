void reset_handler(void);
void can_init(unsigned int kbit);
unsigned int control_step(unsigned int state, int sensor);
int sensor_read(int channel);
void diag_poll(void);
