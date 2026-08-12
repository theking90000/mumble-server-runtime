package be.theking90000.mumble.controller.spaces.load;

enum DriverEventMode {
    FULL,
    MILESTONES;

    static DriverEventMode parse(String value) {
        if ("full".equals(value)) {
            return FULL;
        }
        if ("milestones".equals(value)) {
            return MILESTONES;
        }
        throw new IllegalArgumentException("EVENT_MODE must be full or milestones");
    }
}
