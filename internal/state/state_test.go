package state

import (
	"os"
	"os/exec"
	"sync"
	"testing"
	"time"
)

func TestUpdateSerializesConcurrentWriters(t *testing.T) {
	t.Setenv("XDG_DATA_HOME", t.TempDir())
	start := time.Unix(1_700_000_000, 0).UTC()
	if err := Update(func(st *State) { st.Updater.LastCheckTime = start }); err != nil {
		t.Fatal(err)
	}

	const writers = 32
	var wg sync.WaitGroup
	errs := make(chan error, writers)
	for range writers {
		wg.Add(1)
		go func() {
			defer wg.Done()
			errs <- Update(func(st *State) {
				st.Updater.LastCheckTime = st.Updater.LastCheckTime.Add(time.Second)
			})
		}()
	}
	wg.Wait()
	close(errs)
	for err := range errs {
		if err != nil {
			t.Fatal(err)
		}
	}
	if got, want := Load().Updater.LastCheckTime, start.Add(writers*time.Second); !got.Equal(want) {
		t.Errorf("last check time = %v, want %v", got, want)
	}
}

func TestUpdateSerializesAcrossProcesses(t *testing.T) {
	if os.Getenv("ARGMAX_STATE_UPDATE_HELPER") != "" {
		if err := Update(func(st *State) {
			st.Updater.LastCheckTime = st.Updater.LastCheckTime.Add(time.Second)
		}); err != nil {
			t.Fatal(err)
		}
		return
	}

	t.Setenv("XDG_DATA_HOME", t.TempDir())
	start := time.Unix(1_700_000_000, 0).UTC()
	if err := Update(func(st *State) { st.Updater.LastCheckTime = start }); err != nil {
		t.Fatal(err)
	}
	const writers = 8
	commands := make([]*exec.Cmd, writers)
	for i := range commands {
		commands[i] = exec.Command(os.Args[0], "-test.run=^TestUpdateSerializesAcrossProcesses$")
		commands[i].Env = append(os.Environ(), "ARGMAX_STATE_UPDATE_HELPER=1")
		if err := commands[i].Start(); err != nil {
			t.Fatal(err)
		}
	}
	for _, cmd := range commands {
		if err := cmd.Wait(); err != nil {
			t.Fatalf("state update helper failed: %v", err)
		}
	}
	if got, want := Load().Updater.LastCheckTime, start.Add(writers*time.Second); !got.Equal(want) {
		t.Errorf("last check time = %v, want %v", got, want)
	}
}
