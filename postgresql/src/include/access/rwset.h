#ifndef RWSET_H
#define RWSET_H

#include "utils/relcache.h"
#include "storage/itemptr.h"

extern void CollectReadTID(Relation relation, ItemPointer tid, TransactionId tuple_xid);
extern void SendRWSetAndWaitForCommit(void);
extern void CleanUpRWSet(void);

#endif              /* RWSET_H */