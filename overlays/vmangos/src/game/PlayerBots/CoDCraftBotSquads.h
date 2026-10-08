// Pure population-slot contract, shared by the server and compile-time tests.
#ifndef CODCRAFT_BOT_SQUADS_H
#define CODCRAFT_BOT_SQUADS_H
namespace CoDCraftSquads
{
    constexpr unsigned Index(unsigned slot)
    {
        unsigned index=((slot?slot-1:0)/16)/5;
        return index<30?index:29;
    }
    constexpr unsigned Size=5;
    constexpr unsigned Count=30;
}
#endif
