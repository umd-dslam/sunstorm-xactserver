#include "postgres.h"
#include "postmaster/bgworker.h"
#include "postmaster/remoteworker.h"

int	max_remote_workers = 4;

bool am_remoteworker = false; // Am I a remote worker process?


/* Initialize walsender process before entering the main command loop */
void
InitRemoteWorker(void)
{
	// am_cascading_remoteworker = RecoveryInProgress();

	/* Create a per-remote worker data structure in shared memory */
	// InitRemoteWorkerSlot();

	/*
	 * We don't currently need any ResourceOwner in a remote worker process, but
	 * if we did, we could call CreateAuxProcessResourceOwner here.
	 */

	/*
	 * Let postmaster know that we're a Remote Worker. Once we've declared us as
	 * a Remote Worker process, postmaster will let us outlive the bgwriter and
	 * kill us last in the shutdown sequence, so we get a chance to stream all
	 * remaining WAL at shutdown, including the shutdown checkpoint. Note that
	 * there's no going back, and we mustn't write any WAL records after this.
	 */
	// MarkPostmasterChildRemoteWorker();
	// SendPostmasterSignal(PMSIGNAL_ADVANCE_STATE_MACHINE);

	/* Initialize empty timestamp buffer for lag tracking. */
	// lag_tracker = MemoryContextAllocZero(TopMemoryContext, sizeof(LagTracker));
}


/*
 * RemoteWorkerRegister
 *		Register a background worker running the remote worker.
 */
void
RemoteWorkerRegister(void)
{
	BackgroundWorker bgw;

	if (max_remote_workers == 0)
		return;

	memset(&bgw, 0, sizeof(bgw));
	bgw.bgw_flags = BGWORKER_SHMEM_ACCESS |
		BGWORKER_BACKEND_DATABASE_CONNECTION;
	bgw.bgw_start_time = BgWorkerStart_RecoveryFinished;
	snprintf(bgw.bgw_library_name, BGW_MAXLEN, "remotexact");
	snprintf(bgw.bgw_function_name, BGW_MAXLEN, "RemoteWorkerMain");
	snprintf(bgw.bgw_name, BGW_MAXLEN,
			 "remote worker");
	snprintf(bgw.bgw_type, BGW_MAXLEN,
			 "remote worker");
	bgw.bgw_restart_time = 5;
	bgw.bgw_notify_pid = 0;
	bgw.bgw_main_arg = (Datum) 0;

	RegisterBackgroundWorker(&bgw);
}


/*
 * Execute an incoming remote worker command.
 *
 * Returns true if the cmd_string was recognized as WalSender command, false
 * if not.
 */
bool
exec_remoteworker_command(const char *cmd_string)
{
	return true;
}
