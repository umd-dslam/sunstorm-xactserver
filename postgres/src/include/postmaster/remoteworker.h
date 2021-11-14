/*-------------------------------------------------------------------------
 *
 * remoteworker.h
 *	  Exports from postmaster/remoteworker.c.
 *
 * Portions Copyright (c) 2010-2021, Minghui Liu
 *
 * src/include/postmaster/remoteworker.h
 *
 *-------------------------------------------------------------------------
 */
#ifndef _REMOTEWORKER_H
#define _REMOTEWORKER_H


/* global state */
extern bool am_remoteworker;

extern void InitRemoteWorker(void);
extern void RemoteWorkerRegister(void);
extern bool exec_remoteworker_command(const char *);


#endif							/* _REMOTEWORKER_H */