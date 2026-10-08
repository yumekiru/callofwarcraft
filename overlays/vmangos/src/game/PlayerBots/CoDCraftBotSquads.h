// Pure population-slot contract, shared by the server and compile-time tests.
#ifndef CODCRAFT_BOT_SQUADS_H
#define CODCRAFT_BOT_SQUADS_H
namespace CoDCraftSquads
{
    constexpr unsigned Population(bool elwynn) { return elwynn ? 1200 : 1350; }
    constexpr unsigned GroupSize(unsigned group, bool elwynn) { return elwynn && group==0 ? 75 : 225; }
    constexpr unsigned Index(unsigned slot, bool elwynn=true)
    {
        unsigned ordinal=((slot?slot-1:0)/16)/3;
        unsigned index=ordinal<150 ? ordinal/5 : (ordinal-150)/10+(elwynn?5:0);
        return index<30?index:29;
    }
    constexpr unsigned Size=5;
    constexpr unsigned Count=30;
}
#endif
