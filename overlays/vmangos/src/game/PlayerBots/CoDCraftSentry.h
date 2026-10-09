#ifndef CODCRAFT_SENTRY_H
#define CODCRAFT_SENTRY_H
#include "Common.h"
class Player;
namespace CoDCraftSentry
{
    constexpr uint32 Spell=24733;
    bool Deploy(Player* owner);
    void Update(uint32 diff);
}
#endif
